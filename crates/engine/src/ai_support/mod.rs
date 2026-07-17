mod candidates;
mod context;
mod copy;
pub mod filter;

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::game::casting;
use crate::game::casting_costs;
use crate::game::layers;
use crate::game::mana_abilities;
use crate::game::mana_payment;
use crate::game::mana_sources;
use crate::game::triggers;
use crate::types::ability::{AbilityKind, CounterCostSelection, TriggerDefinition};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    AutoMayChoice, CastOfferKind, GameState, MulliganDecisionPhase, PayCostKind,
    PendingMulliganAction, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

pub use candidates::{
    candidate_actions, candidate_actions_broad, candidate_actions_exact,
    candidate_actions_with_probe, ActionMetadata, CandidateAction, TacticalClass,
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
    validated_candidate_actions_with_probe(state, None)
}

pub fn validated_candidate_actions_with_probe(
    state: &GameState,
    probe: Option<&crate::game::casting::PriorityCastProbe>,
) -> Vec<CandidateAction> {
    let pipeline = FilterPipeline::default_pipeline();
    let mut actions =
        pipeline.apply_with_probe(state, candidate_actions_with_probe(state, probe), probe);
    // Issue #4878: candidate enumeration must not depend on HashSet/HashMap
    // iteration order leaking into AI tie-breaking downstream. Ordered via the
    // allocation-free `GameAction::cmp_stable` total order (not `Debug` strings).
    actions.sort_by(|a, b| a.action.cmp_stable(&b.action));
    actions
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
        ConvokeMode::Delve => obj.is_delve_eligible(*player) && *mana_type == ManaType::Colorless,
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
        | (WaitingFor::Priority { .. }, GameAction::TurnFaceUp { object_id, .. })
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
        ) => costs.get(*index).is_none_or(|cost| {
            !casting::can_pay_ability_cost_now(
                state,
                *player,
                pending_cast.object_id,
                cost,
                pending_cast.ability.context.ability_tag,
            )
        }),
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
        | (
            WaitingFor::CastOffer {
                kind: CastOfferKind::GraveyardPaidCast { .. },
                ..
            },
            GameAction::GraveyardPaidCastChoice { .. },
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
        (WaitingFor::MulliganDecision { pending, .. }, GameAction::SelectCards { cards }) => {
            pending.iter().all(|entry| match &entry.phase {
                MulliganDecisionPhase::Declare => true,
                MulliganDecisionPhase::BottomCards { count, then } => {
                    let excluded_hit = matches!(
                        then,
                        PendingMulliganAction::UseSerumPowder { object_id } if cards.contains(object_id)
                    );
                    excluded_hit
                        || selection_mismatch(
                            cards,
                            &state.players[entry.player.0 as usize].hand,
                            Some((*count).into()),
                        )
                }
            })
        }
        (WaitingFor::OpeningHandBottomCards { pending, .. }, GameAction::SelectCards { cards }) => {
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
                || mana_sources::object_mana_ability_penalty(state, source_id, ability)
                    .is_meaningful_priority_activation()
        })
    })
}

fn land_mana_options_for_priority(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    aura_sources: &[ObjectId],
    mana_activation_gates: &mana_abilities::ManaActivationGates,
) -> Vec<mana_sources::ManaSourceOption> {
    mana_sources::activatable_land_mana_options_indexed_gated(
        state,
        object_id,
        player,
        aura_sources,
        mana_activation_gates,
    )
}

fn resolve_mana_option_for_trigger_probe(
    state: &GameState,
    player: PlayerId,
    option: &mana_sources::ManaSourceOption,
) -> bool {
    let mut probe = state.clone();
    let mut events = Vec::new();

    for (aura_id, override_value) in &option.taps_for_mana_overrides {
        probe
            .pending_taps_for_mana_overrides
            .insert(*aura_id, override_value.clone());
    }

    if let Some(ability_index) = option.ability_index {
        let Some(ability_def) = probe
            .objects
            .get(&option.object_id)
            .and_then(|obj| obj.abilities.get(ability_index))
            .cloned()
        else {
            return false;
        };
        let override_value = casting_costs::production_override_for_option(&ability_def, option);
        if mana_abilities::resolve_mana_ability(
            &mut probe,
            option.object_id,
            player,
            &ability_def,
            &mut events,
            override_value,
        )
        .is_err()
        {
            return false;
        }
    } else {
        if let Some(obj) = probe.objects.get_mut(&option.object_id) {
            if !obj.tapped {
                obj.tapped = true;
                events.push(GameEvent::PermanentTapped {
                    object_id: option.object_id,
                    caused_by: None,
                });
            }
        }
        mana_payment::produce_mana(
            &mut probe,
            option.object_id,
            option.mana_type,
            player,
            true,
            &mut events,
        );
        events.push(GameEvent::TappedForMana {
            player_id: player,
            source_id: option.object_id,
            produced: vec![option.mana_type],
            tap_state: ManaTapState::FromTap,
        });
    }

    triggers::events_would_queue_non_mana_trigger(&mut probe, &events)
}

fn activate_mana_action_would_queue_non_mana_trigger(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    aura_sources: &[ObjectId],
    mana_activation_gates: &mana_abilities::ManaActivationGates,
) -> bool {
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };

    if obj.card_types.core_types.contains(&CoreType::Land) {
        let options = land_mana_options_for_priority(
            state,
            player,
            source_id,
            aura_sources,
            mana_activation_gates,
        );
        let matching_options = options
            .iter()
            .filter(|option| option.ability_index == Some(ability_index))
            .collect::<Vec<_>>();
        if !matching_options.is_empty() {
            return matching_options
                .iter()
                .any(|option| resolve_mana_option_for_trigger_probe(state, player, option));
        }
    }

    let Some(ability_def) = obj.abilities.get(ability_index).cloned() else {
        return false;
    };
    let mut probe = state.clone();
    let mut events = Vec::new();
    if mana_abilities::resolve_mana_ability(
        &mut probe,
        source_id,
        player,
        &ability_def,
        &mut events,
        None,
    )
    .is_err()
    {
        return false;
    }
    triggers::events_would_queue_non_mana_trigger(&mut probe, &events)
}

fn tap_land_action_would_queue_non_mana_trigger(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    aura_sources: &[ObjectId],
    mana_activation_gates: &mana_abilities::ManaActivationGates,
) -> bool {
    land_mana_options_for_priority(
        state,
        player,
        object_id,
        aura_sources,
        mana_activation_gates,
    )
    .iter()
    .any(|option| {
        option.penalty.is_meaningful_priority_activation()
            || resolve_mana_option_for_trigger_probe(state, player, option)
    })
}

fn grouped_mana_requires_priority(state: &GameState, player: PlayerId) -> bool {
    let aura_sources = mana_sources::taps_for_mana_trigger_sources(state);
    let mana_activation_gates = mana_abilities::ManaActivationGates::compute(state);

    activatable_object_mana_actions_for_player(state, player)
        .iter()
        .any(|action| match action {
            GameAction::TapLandForMana { object_id } => {
                tap_land_action_would_queue_non_mana_trigger(
                    state,
                    player,
                    *object_id,
                    &aura_sources,
                    &mana_activation_gates,
                )
            }
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => activate_mana_action_would_queue_non_mana_trigger(
                state,
                player,
                *source_id,
                *ability_index,
                &aura_sources,
                &mana_activation_gates,
            ),
            _ => false,
        })
}

/// The flat-list half of [`has_meaningful_priority_action`]: any non-pass,
/// non-standalone-mana priority action in the caller-supplied `actions` list
/// (or a sacrifice-for-mana injected into the flat list by a test/probe).
/// Extracted so the auto-pass gate and the CR 732.5 loop-shortcut firewall
/// classifier consume the SAME primitive and cannot drift.
fn flat_actions_have_meaningful_priority(state: &GameState, actions: &[GameAction]) -> bool {
    actions.iter().any(|action| match action {
        GameAction::PassPriority => false,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => activate_ability_is_meaningful_priority(state, *source_id, *ability_index),
        _ => true,
    })
}

/// G2 upkeep/draw gate: like [`flat_actions_have_meaningful_priority`] but a
/// merely-castable spell (`CastSpell`) does NOT count. Per the locked design
/// decision, a castable instant at your own upkeep/draw keeps auto-passing
/// (MTGA parity — see the existing
/// `auto_passes_initial_upkeep_and_draw_priority_with_instant_speed_actions`
/// regression); only a genuine non-cast action (a meaningful activated ability,
/// morph flip, and other non-cast special actions) holds the initial
/// upkeep/draw window open. Kept SEPARATE from
/// `flat_actions_have_meaningful_priority` because that primitive is wired into
/// the CR 732.5 loop-detection firewall (`has_meaningful_priority_action`) and
/// must stay byte-identical. Delegates to the same
/// `activate_ability_is_meaningful_priority` classifier — the only added logic
/// is the explicit `CastSpell => false` arm.
fn flat_actions_have_meaningful_noncast_priority(
    state: &GameState,
    actions: &[GameAction],
) -> bool {
    actions.iter().any(|action| match action {
        GameAction::PassPriority => false,
        GameAction::CastSpell { .. } => false,
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => activate_ability_is_meaningful_priority(state, *source_id, *ability_index),
        _ => true,
    })
}

/// Issue #544: sacrifice-for-mana abilities (KCI, Phyrexian Altar, etc.) are
/// grouped-only (absent from the flat `legal_actions` list) but remain a real
/// board-changing action. Structural, state-driven scan — no downstream-use
/// judgement here (that belongs to the auto-pass gate, not the loop firewall).
fn has_activatable_sacrifice_for_mana(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. })
        && mana_actions_include_meaningful_sacrifice(state, &activatable_object_mana_actions(state))
}

/// Slice-taking core of [`has_activatable_sacrifice_for_mana`]: given a
/// precomputed activatable mana-action sweep, true iff any action is a
/// meaningful (sacrifice-for-mana) mana activation. Extracted so
/// `auto_pass_recommended` can compute the sweep ONCE and share it between the
/// G1 beneficial-mana-tap hold (rung 5) and this rung-9 sac check, avoiding the
/// PR #5229 double-evaluation of the mana-action sweep.
fn mana_actions_include_meaningful_sacrifice(
    state: &GameState,
    object_mana_actions: &[GameAction],
) -> bool {
    object_mana_actions.iter().any(|action| {
        matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if activate_ability_is_meaningful_priority(state, *source_id, *ability_index)
        )
    })
}

/// True when `actions` contains a priority action that materially changes the
/// game beyond passing or producing standalone mana.
///
/// Sacrifice-for-mana activations are omitted from the flat `legal_actions`
/// list (they live in `legal_actions_by_object` only) but remain meaningful
/// priority decisions, so during `WaitingFor::Priority` this also scans
/// `activatable_object_mana_actions`. Recomposed from the two building blocks
/// above so the auto-pass gate reuses the identical primitives — behavior is
/// byte-identical to the previous inline form (loop-firewall safe).
pub fn has_meaningful_priority_action(state: &GameState, actions: &[GameAction]) -> bool {
    flat_actions_have_meaningful_priority(state, actions)
        || has_activatable_sacrifice_for_mana(state)
}

/// PR-7 Phase 4c (LOW-2): CR 732.2b/c + CR 104.4b — an AI opponent polled on an
/// OPTIONAL loop shortcut. The offer is raised only for optional loops the polled
/// player CAN break (a Path A optional drain, `WaitingFor::LoopShortcut`); the win
/// is a loss condition for every polled opponent (single-faller: see
/// `interactive_loop_bridge`'s Path A gate, CR 104.2a). SELF-PRESERVATION: if this
/// player has a meaningful priority action (a way to break the loop), name
/// `Shorten{0}` — the engine realizes Shorten as a real `WaitingFor::Priority`
/// window (`game::engine::apply_action`'s `RespondToShortcut(Shorten)` arm) where
/// it can act — rather than Accept its own loss. No meaningful action ⇒ Accept
/// (nothing to do). Single authority for all 3 `RespondToShortcut` emission sites
/// (engine `candidates.rs` + phase-ai `projection.rs`/`search.rs`) so the
/// self-preservation heuristic can't drift between them. Reuses the exact
/// `no_living_player_has_meaningful_priority_action` probe recipe
/// (`game::engine`), scoped to the single polled player.
pub fn smart_shortcut_response(
    state: &GameState,
    polled_player: PlayerId,
) -> crate::analysis::loop_check::ShortcutResponse {
    let mut probe_state = state.clone();
    probe_state.auto_pass.clear();
    probe_state.priority_player = polled_player;
    probe_state.waiting_for = WaitingFor::Priority {
        player: polled_player,
    };
    layers::flush_layers(&mut probe_state);
    let probe = casting::PriorityCastProbe::from_flushed_state(probe_state, polled_player);
    let actions = flat_priority_actions_with_probe(probe.state(), Some(&probe));
    if has_meaningful_priority_action(probe.state(), &actions) {
        // CR 732.2b: name an earlier stopping point — take my window instead of losing.
        crate::analysis::loop_check::ShortcutResponse::Shorten { at_iteration: 0 }
    } else {
        // CR 732.2c: nothing to do — agree to take the shortcut.
        crate::analysis::loop_check::ShortcutResponse::Accept
    }
}

fn auto_passes_initial_priority_by_default(state: &GameState) -> bool {
    state.stack.is_empty() && matches!(state.phase, Phase::Upkeep | Phase::Draw)
}

/// CR 117.1d + CR 601.2g: True when the player has a spell the castability gate
/// accepts via manual mana-ability payment even though the simulation oracle
/// (`SimulationFilter`) rejects the Auto-mode `CastSpell` candidate (issue #562,
/// #583). Frontends surface these via `spell_costs` + manual cast dispatch.
fn has_feasibly_castable_spell(
    state: &GameState,
    player: PlayerId,
    probe: Option<&crate::game::casting::PriorityCastProbe>,
) -> bool {
    crate::game::casting::spell_objects_available_to_cast(state, player)
        .iter()
        .any(|&object_id| {
            crate::game::casting::can_cast_object_now_with_probe(state, player, object_id, probe)
        })
}

/// CR 605.3a + CR 603.6 + CR 117.1d: A sacrifice-for-mana activation blocks
/// frontend auto-pass ONLY when the sacrifice or its mana enables a concrete
/// follow-up. Case (1) — a feasibly castable spell — is already resolved by the
/// castability rungs above `auto_pass_recommended`'s sac branch (both count
/// sac-for-mana mana via `feasible_mana_capacity`), so it is structurally
/// `false` here and intentionally NOT re-checked (hot-path perf — re-calling
/// `has_feasibly_castable_spell` would repeat the whole hand sweep). This gate
/// covers the two remaining downstream channels:
///   (2) a mana-costed non-mana activated ability the produced mana could feed, and
///   (3) a leaves-the-battlefield / dies / sacrifice trigger (CR 603.6c) where the
///       sacrifice itself is the payoff (aristocrats / altars).
/// Both are conservative presence checks (hold on presence, not on proven net
/// benefit) — they err toward HOLDING (today's behavior), so no real play is
/// ever silently auto-passed. This is a live query over current game state; it
/// snapshots nothing.
fn sacrifice_for_mana_enables_followup(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        let Some(obj) = state.objects.get(id) else {
            return false;
        };
        if obj.controller != player {
            return false;
        }
        // Case (2): a non-mana activated ability with a mana component in its
        // cost — the produced mana could pay it (CR 602.2: activating an ability
        // is putting it on the stack and paying its costs).
        let case2 = obj.abilities.iter().any(|ability| {
            ability.kind == AbilityKind::Activated
                && !mana_abilities::is_mana_ability(ability)
                && mana_abilities::mana_sub_cost_of(&ability.cost).is_some()
        });
        // Case (3): a trigger that fires when a controlled permanent leaves the
        // battlefield — the sacrifice is itself the payoff (CR 603.6c).
        let case3 = obj
            .trigger_definitions
            .iter_all()
            .any(trigger_fires_on_leaving_battlefield);
        case2 || case3
    })
}

/// CR 603.6c + CR 701.21: True when this trigger fires on a permanent moving off
/// the battlefield (leaves / dies / sacrificed / destroyed). Bounded structural
/// predicate over `TriggerMode` plus the zone-change source constraint.
fn trigger_fires_on_leaving_battlefield(trigger: &TriggerDefinition) -> bool {
    use crate::types::triggers::TriggerMode::*;
    match trigger.mode {
        // CR 603.6c / CR 701.21 / CR 701.8: direct leaves-the-battlefield,
        // sacrifice, and destroy triggers always fire on a battlefield exit.
        LeavesBattlefield | Sacrificed | SacrificedOnce | Destroyed => true,
        // A general zone-change trigger is a battlefield-exit payoff only when
        // its source-zone constraint admits the battlefield. `matches_from`
        // answers "would an object leaving the battlefield satisfy this origin?"
        // — correctly rejecting `NotEquals(Battlefield)` ("from anywhere other
        // than the battlefield") while admitting `Any` / `Equals(Battlefield)` /
        // `OneOf([.. Battlefield ..])` and the disjunctive `zone_change_clauses`
        // shape (Syr Konrad: the leaves-battlefield event lives only in a clause,
        // not the scalar `origin`/`origin_zones`).
        ChangesZone | ChangesZoneAll => {
            trigger.origin == Some(Zone::Battlefield)
                || trigger.origin_zones.contains(&Zone::Battlefield)
                || trigger
                    .zone_change_clauses
                    .iter()
                    .any(|clause| clause.origin.matches_from(&Some(Zone::Battlefield)))
        }
        // All other modes (enters, cast, damage, attack, tap, counters, life, …)
        // are NOT battlefield-exit events — a sacrifice does not feed them.
        _ => false,
    }
}

/// G1 — stage 2 of the beneficial mana-tap trigger hold (CR 605.1b window).
///
/// The caller has already confirmed the cheap stage-1 gate (own-turn,
/// empty-stack main phase) and that `beneficial_sources` — the player's
/// permanents carrying a *beneficial* non-mana `TapsForMana` / `ManaAdded`
/// trigger — is non-empty. This stage sweeps the (already-computed, §C.3-shared)
/// activatable mana-action list and returns `true` iff tapping one of the
/// player's own currently-activatable mana sources would actually FIRE such a
/// trigger (CR 106.12a). Post-filter this is hold-on-presence again: a single
/// firing beneficial trigger justifies holding priority.
///
/// Self-quiescing: once the source is tapped it drops out of the mana-action
/// sweep, so the hold releases on the next call (no infinite hold).
fn beneficial_mana_tap_trigger_hold(
    state: &GameState,
    player: PlayerId,
    object_mana_actions: &[GameAction],
    beneficial_sources: &[ObjectId],
) -> bool {
    object_mana_actions.iter().any(|action| {
        // CR 106.12a: only a mana ability whose activation cost includes {T}
        // emits the `TappedForMana` event these triggers key off. The
        // `TapLandForMana` shortcut is always a single {T} option (its tap
        // component is trivially satisfied); an `ActivateAbility` may be a
        // non-tap mana ability, so consult the ability's cost.
        let mana_source = match action {
            GameAction::TapLandForMana { object_id } => *object_id,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                let has_tap = state
                    .objects
                    .get(source_id)
                    .and_then(|obj| obj.abilities.get(*ability_index))
                    .is_some_and(|ability| mana_sources::has_tap_component(&ability.cost));
                if !has_tap {
                    return false;
                }
                *source_id
            }
            _ => return false,
        };

        beneficial_sources.iter().any(|&trigger_source| {
            let Some(obj) = state.objects.get(&trigger_source) else {
                return false;
            };
            obj.trigger_definitions.iter_all().any(|trigger| {
                if !mana_sources::is_non_mana_tap_trigger(trigger)
                    || !mana_sources::trigger_chain_benefits_controller(trigger)
                {
                    return false;
                }
                match trigger.mode {
                    // CR 106.12a: the card-identity + tapping-player authority is
                    // `taps_for_mana_card_matches` + `valid_player_matches`, the
                    // same predicates the trigger resolver uses. `taps_for_mana_card_matches`
                    // ignores `taps_for_mana_produced`, so a produced-mana filter is
                    // treated as matching (over-approx, err-to-hold).
                    crate::types::triggers::TriggerMode::TapsForMana => {
                        crate::game::trigger_matchers::taps_for_mana_card_matches(
                            trigger,
                            state,
                            mana_source,
                            trigger_source,
                        ) && crate::game::trigger_matchers::valid_player_matches(
                            trigger,
                            state,
                            player,
                            trigger_source,
                        )
                    }
                    // CR 605.1b: a `ManaAdded` trigger has no card filter
                    // (`match_mana_added` matches any mana-added event), so it fires
                    // on any own mana activation — over-approximate to firing
                    // (err-to-hold; zero such beneficial cards printed today).
                    crate::types::triggers::TriggerMode::ManaAdded => {
                        crate::game::trigger_matchers::valid_player_matches(
                            trigger,
                            state,
                            player,
                            trigger_source,
                        )
                    }
                    _ => false,
                }
            })
        })
    })
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
    let flushed;
    let state = if state.layers_dirty.is_dirty() {
        flushed = {
            let mut state = state.clone();
            layers::flush_layers(&mut state);
            state
        };
        &flushed
    } else {
        state
    };

    let player = match &state.waiting_for {
        WaitingFor::Priority { player } => *player,
        _ => return false,
    };

    // Lazy, compute-once locals shared across rungs (§C):
    //  - `cast_probe`: one `PriorityCastProbe` built when the first castability
    //    rung is reached (rungs 7/8 are mutually exclusive on `active_player`,
    //    so it is built at most once), reused so the auto-tap source cache and
    //    layer flush are not rebuilt per candidate spell.
    //  - `object_mana_actions`: one activatable mana-action sweep shared by the
    //    G1 beneficial-tap hold (rung 5) and the rung-9 sac-for-mana check,
    //    avoiding the PR #5229 double-evaluation.
    let mut cast_probe: Option<crate::game::casting::PriorityCastProbe> = None;
    let mut object_mana_actions: Option<Vec<GameAction>> = None;
    let mut grouped_mana_priority: Option<bool> = None;

    // A phase stop on the current phase (empty stack = initial priority window)
    // means the player asked to pause here — never recommend auto-pass. Moved
    // from the frontend so the engine is the single authority. Disjoint from the
    // CR 117.3d yield short-circuit below, which requires a NON-empty stack
    // (`stack.back()` is `Some`); this branch requires an EMPTY stack.
    //
    // Seat note: this gate keys on the `WaitingFor::Priority` player bound
    // above; the frontend gate it replaced keyed on `state.priority_player`.
    // CR 723.5: while controlling another player, one player makes all of that
    // player's choices — so the priority holder and `priority_player` can be
    // different seats. With an empty stack those two seats diverge only in that
    // turn-control case, and this divergence is accepted (the checked seat is
    // the one actually being asked to act).
    if state.stack.is_empty() && state.phase_stop_hit(player) {
        return false;
    }

    // CR 117.3d: A standing priority yield for the top-of-stack trigger is an
    // explicit pre-commitment to pass. It deliberately overrides the castability
    // and meaningful-action holds below (including the issue #4388 opponent-turn
    // mana window) — the player has already decided not to interact with this
    // trigger class, so recommend auto-pass regardless of what they could cast.
    if state
        .stack
        .back()
        .is_some_and(|top| state.is_priority_yielded(player, top))
    {
        return true;
    }

    // Rung 4 — MTGA-style: auto-pass when the player's own spell/ability is on
    // top of the stack. The player almost never wants to respond to their own
    // spell — let it resolve. Hoisted above the castability holds (G3): accepted
    // MTGA parity means an own object on top outranks "you could still cast
    // something", including the case where the top object is your own triggered
    // ability (own triggers lose their implicit stop). Kept BELOW the CR 117.3d
    // yield rung so an explicit yield still wins, and disjoint from the G1 rung
    // below (that requires an empty stack; this requires a non-empty one).
    // Full control mode (checked by the frontend) overrides this.
    if let Some(top) = state.stack.back() {
        if top.controller == player {
            return true;
        }
    }

    // Rung 5 — G1 (CR 605.1b + CR 603.3): hold priority when tapping one of the
    // player's own currently-activatable mana sources would fire a *beneficial*
    // non-mana tap trigger (opponent-scoped damage/life-loss or controller-scoped
    // life gain — Zhur-Taa Druid's "deals 1 damage to each opponent"). Such a
    // trigger USES THE STACK (it fails CR 605.1b's mana-ability test), so a real
    // priority window exists to hold in. Scoped to own-turn, empty-stack MAIN
    // phases (the deliberate residual instant-speed windows delegate to phase
    // stops). Stage 1 is a clone-free AST gate (§C.2); the mana-action sweep is
    // computed lazily only past the gate and shared with the rung-9 sac check.
    if state.stack.is_empty()
        && state.active_player == player
        && matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
    {
        let beneficial_sources =
            mana_sources::beneficial_non_mana_tap_trigger_sources(state, player);
        if !beneficial_sources.is_empty() {
            let sweep =
                object_mana_actions.get_or_insert_with(|| activatable_object_mana_actions(state));
            if beneficial_mana_tap_trigger_hold(
                state,
                player,
                sweep.as_slice(),
                &beneficial_sources,
            ) {
                return false;
            }
        }
    }

    // CR 117.1d (issue #4388): On an opponent's turn the priority player may
    // hold to cast an instant/flash spell paid for with their own mana
    // abilities (Gaea's Cradle, Itlimoc, basic lands). Hold ONLY when they
    // actually have a feasibly castable spell to spend that mana on — a bare
    // mana source with nothing castable is not a meaningful priority window, so
    // let auto-pass fire. `has_feasibly_castable_spell` covers the full
    // activatable-capacity space (auto-tap AND manual-float payment,
    // game/casting.rs:11375-11461), so it subsumes the old
    // `activatable_object_mana_actions` proxy while dropping the false HOLD.
    // Meaningful non-mana activated abilities, grouped mana that would queue
    // non-mana triggers, and issue #544 sac-for-mana on an opponent's turn are
    // still held below by the meaningful-action/sac gates; a dedicated
    // `has_feasibly_activatable_ability` opponent-turn seam (the ability
    // analogue of this predicate) is deferred as future work.
    if state.active_player != player {
        let probe: &_ = cast_probe.get_or_insert_with(|| {
            crate::game::casting::PriorityCastProbe::from_flushed_state(state.clone(), player)
        });
        if has_feasibly_castable_spell(probe.state(), player, Some(probe)) {
            return false;
        }
        if *grouped_mana_priority
            .get_or_insert_with(|| grouped_mana_requires_priority(state, player))
        {
            return false;
        }
    }

    // Own-turn upkeep/draw fast path (G2, gated). This MUST stay above the
    // own-turn castability rung so a routine own upkeep/draw window
    // short-circuits here BEFORE any hand sweep (perf; preserves today's
    // behavior). The rung above is gated on `active_player != player` and the
    // rung below on `active_player == player`, so the two are mutually
    // exclusive: `has_feasibly_castable_spell` runs at most once per call, and
    // zero times on your own upkeep/draw.
    //
    // Gated (G2) so it fast-passes ONLY a genuinely empty window:
    //  - `active_player == player`: the "own upkeep/draw" intent this rung
    //    claims. On an OPPONENT's upkeep/draw the meaningful-action holds below
    //    must still apply, so this fast path must not fire there.
    //  - `!flat_actions_have_meaningful_noncast_priority`: a meaningful NON-cast
    //    flat action (a meaningful activated ability, morph flip, other special
    //    actions) at your own upkeep/draw is a real decision — hold for it. A
    //    merely-castable instant (`CastSpell`) deliberately does NOT hold here
    //    (MTGA parity — see the noncast predicate's doc and the
    //    `auto_passes_initial_upkeep_and_draw_priority_with_instant_speed_actions`
    //    regression). The distinct noncast predicate is used (not
    //    `flat_actions_have_meaningful_priority`) precisely so casts pass while
    //    the CR 732.5 loop-firewall primitive stays byte-identical.
    if state.active_player == player
        && auto_passes_initial_priority_by_default(state)
        && !flat_actions_have_meaningful_noncast_priority(state, actions)
    {
        return true;
    }

    if state.active_player == player {
        let probe: &_ = cast_probe.get_or_insert_with(|| {
            crate::game::casting::PriorityCastProbe::from_flushed_state(state.clone(), player)
        });
        if has_feasibly_castable_spell(probe.state(), player, Some(probe)) {
            return false;
        }
    }

    // A genuinely meaningful non-mana priority action always holds. Grouped
    // mana holds when activating it would queue a non-mana trigger. A
    // sacrifice-for-mana ability (issue #544) holds ONLY when the sacrifice or
    // its mana enables a concrete downstream follow-up — case (1) a feasibly
    // castable spell is already resolved by the castability rungs above, so
    // cases (2) mana-costed activated ability and (3) leaves-battlefield trigger
    // are checked here (CR 605.3a + CR 603.6). A bare sac-for-mana source with
    // no downstream use is not a meaningful priority window — let auto-pass fire.
    // Short-circuit order preserves perf: the followup scan runs only when the
    // flat list is not already meaningful AND a sac-for-mana source is present.
    let holds = flat_actions_have_meaningful_priority(state, actions)
        || *grouped_mana_priority
            .get_or_insert_with(|| grouped_mana_requires_priority(state, player))
        || {
            // Short-circuit: only sweep (or reuse the rung-5 sweep) when the flat
            // list is not already meaningful AND a sac-for-mana source is present.
            let sweep =
                object_mana_actions.get_or_insert_with(|| activatable_object_mana_actions(state));
            mana_actions_include_meaningful_sacrifice(state, sweep.as_slice())
                && sacrifice_for_mana_enables_followup(state, player)
        };
    if !holds {
        return true;
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

/// The flat priority-action list: validated candidate actions minus mana
/// abilities. This is the single authority for the non-target-selection action
/// body so the auto-pass probe (`priority_player_has_meaningful_action`) and
/// `legal_actions_full` cannot drift. Auto-pass consumes only this list; it does
/// not need the spell-cost map or the grouped per-object map that
/// `legal_actions_full` additionally builds.
pub fn flat_priority_actions(state: &GameState) -> Vec<GameAction> {
    flat_priority_actions_with_probe(state, None)
}

pub fn flat_priority_actions_with_probe(
    state: &GameState,
    probe: Option<&crate::game::casting::PriorityCastProbe>,
) -> Vec<GameAction> {
    validated_candidate_actions_with_probe(state, probe)
        .into_iter()
        .map(|candidate| candidate.action)
        .filter(|action| !action.is_mana_ability())
        .collect()
}

/// Returns legal actions, spell costs, AND a per-permanent action grouping.
///
/// `legal_actions_by_object` maps each permanent (or hand-zone card) to the
/// engine-authoritative actions the frontend may offer for that object. The
/// grouped map includes mana actions that are intentionally absent from the
/// flat `actions` list; auto-pass consumes the flat list, while board
/// interaction consumes the grouped map.
pub fn legal_actions_full(state: &GameState) -> LegalActionsFull {
    let priority_probe_storage;
    let flushed_storage;
    let (state, priority_probe) = match &state.waiting_for {
        WaitingFor::Priority { player } => {
            priority_probe_storage = if state.layers_dirty.is_dirty() {
                let mut flushed = state.clone();
                layers::flush_layers(&mut flushed);
                crate::game::casting::PriorityCastProbe::from_flushed_state(flushed, *player)
            } else {
                crate::game::casting::PriorityCastProbe::new(state, *player)
            };
            (
                priority_probe_storage.state(),
                Some(&priority_probe_storage),
            )
        }
        _ if state.layers_dirty.is_dirty() => {
            flushed_storage = {
                let mut state = state.clone();
                layers::flush_layers(&mut state);
                state
            };
            (&flushed_storage, None)
        }
        _ => (state, None),
    };

    let mut actions: Vec<GameAction> = target_selection_actions_without_simulation(state)
        .unwrap_or_else(|| flat_priority_actions_with_probe(state, priority_probe));

    // This preference-setting action is intentionally excluded from AI candidate
    // generation: it changes future prompt behavior rather than making a tactical
    // game decision. It remains a legal player action, however, and must be present
    // in the engine snapshot so a queued UI choice is not discarded as stale.
    if matches!(
        &state.waiting_for,
        WaitingFor::OptionalEffectChoice {
            may_trigger_key: Some(_),
            ..
        }
    ) {
        actions.extend([
            GameAction::DecideOptionalEffectAndRemember {
                choice: AutoMayChoice::Accept,
            },
            GameAction::DecideOptionalEffectAndRemember {
                choice: AutoMayChoice::Decline,
            },
        ]);
    }

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
        crate::game::perf_counters::record_legal_actions_spell_cost_sweep();
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
    // OpeningHandBottomCards), every pending player has a
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
    // Loop-invariant hoist: the TapsForMana trigger-source list is identical for
    // every land in this board-global sweep, so compute it once instead of
    // re-scanning the whole battlefield per land inside `land_mana_options`.
    let aura_sources = mana_sources::taps_for_mana_trigger_sources(state);
    let mana_activation_gates = mana_abilities::ManaActivationGates::compute(state);
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
            let options = mana_sources::activatable_land_mana_options_indexed_gated(
                state,
                obj_id,
                player,
                &aura_sources,
                &mana_activation_gates,
            );
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
                && mana_abilities::can_activate_mana_ability_now_gated(
                    state,
                    player,
                    obj_id,
                    idx,
                    ability,
                    &mana_activation_gates,
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
    use crate::game::mana_sources;
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ContinuousModification,
        ControllerRef, Effect, FilterProp, ManaContribution, ManaProduction, QuantityExpr,
        ResolvedAbility, SacrificeCost, SearchSelectionConstraint, StaticDefinition, TargetFilter,
        TargetRef, TriggerDefinition, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        CastingVariant, ConvokeMode, DistributionUnit, GameState, MulliganDecisionEntry,
        MulliganDecisionPhase, PendingCast, PendingMulliganAction, StackEntry, StackEntryKind,
        WaitingFor,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::{Keyword, KeywordKind};
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
    use crate::types::phase::{Phase, PhaseStop, PhaseStopScope};
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
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

    fn setup_opponent_priority(phase: Phase) -> GameState {
        let mut state = setup_priority();
        state.phase = phase;
        state.active_player = PlayerId(1);
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

    fn add_mana_trigger_source(
        state: &mut GameState,
        name: &str,
        mode: TriggerMode,
        effect: Effect,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(9100),
            PlayerId(1),
            name.to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.trigger_definitions.push(
                TriggerDefinition::new(mode)
                    .execute(AbilityDefinition::new(AbilityKind::Database, effect))
                    .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))),
            );
        }
        source
    }

    fn draw_controller_effect() -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    fn add_green_mana_effect() -> Effect {
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Additional,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        }
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
            attacker_constraints: Default::default(),
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
    fn legal_actions_offer_cancel_cast_during_distribution_with_unpayable_pending_cost() {
        let mut state = setup_priority();
        set_dummy_pending_cast(&mut state);
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::DistributeAmong {
            player: PlayerId(0),
            total: 1,
            targets: vec![TargetRef::Object(target)],
            unit: DistributionUnit::Damage,
        };

        let actions = legal_actions(&state);

        assert!(
            actions.contains(&GameAction::CancelCast),
            "pending distribution must remain cancellable when the distribution candidate is rejected"
        );
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
            candidates: Vec::new(),
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
            library_owner: None,
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
            library_owner: None,
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
            library_owner: None,
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
                    recipient: TargetFilter::SelfRef,
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

    // Issue #544 (narrowed): Krark-Clan Ironworks ("Sacrifice an artifact: Add
    // {C}{C}") is a mana ability whose cost sacrifices a permanent. It remains a
    // meaningful priority decision for the CR 732.5 loop firewall (the classifier
    // `has_meaningful_priority_action` still returns true — the sacrifice is a
    // board change). But with a BARE board (no castable spell, no mana-costed
    // activated ability, no leaves-battlefield trigger) the sacrifice enables no
    // concrete follow-up, so the frontend auto-pass recommendation now fires.
    // Classifier and recommendation deliberately DISAGREE for this bare case.
    #[test]
    fn auto_pass_fires_for_bare_sac_for_mana_own_turn() {
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
        // Classifier reach-guard: the sac-for-mana is still a meaningful
        // priority decision for the loop firewall (CR 605.3a + 603.6) — this
        // assertion is UNCHANGED and proves the classifier and the auto-pass
        // recommendation deliberately disagree for the bare case below.
        assert!(
            super::has_meaningful_priority_action(&state, &actions),
            "A sacrifice-for-mana ability is a meaningful priority decision (CR 605.3a + 603.6)"
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
        // NARROWED (this task): with a bare board the grouped sac-for-mana
        // enables no downstream follow-up, so auto-pass now FIRES even though the
        // classifier above still counts it as meaningful. This is the flipped
        // assertion — it fails if the narrowing is reverted.
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "Bare sac-for-mana with no downstream castable spell / mana-costed \
             ability / leaves-battlefield trigger → auto-pass fires"
        );
    }

    /// Issue #4388 (narrowed): a lone mana source (Gaea's Cradle / Itlimoc /
    /// basic land) with nothing castable to spend the mana on is NOT a
    /// meaningful priority window — auto-pass fires even on an opponent's turn
    /// (CR 117.1d covers *permission* to activate, not an obligation to stop).
    #[test]
    fn auto_pass_releases_priority_for_lone_mana_on_opponents_turn() {
        use crate::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);

        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let state = runner.state();

        // Reach-guard FIRST: prove the Forest's mana ability is genuinely
        // activatable on the opponent's turn, so this test exercises the
        // lone-mana branch rather than passing vacuously.
        assert!(
            !super::activatable_object_mana_actions(state).is_empty(),
            "precondition: Forest mana is activatable"
        );
        assert!(
            super::auto_pass_recommended(state, &super::flat_priority_actions(state)),
            "lone mana source with empty hand on opponent's turn → auto-pass"
        );
    }

    /// Issue #4388: on an opponent's turn, a feasibly castable instant paired
    /// with the mana to cast it is a meaningful priority window — hold (CR
    /// 117.1d + CR 601.2g: mana abilities feed the pending mana payment).
    #[test]
    fn auto_pass_holds_priority_for_castable_spell_on_opponents_turn() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test Bolt", true, "Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            });

        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }

        // Positive reach-guard: prove the {G} instant is feasibly castable so
        // the HOLD assertion below cannot pass vacuously.
        assert!(
            super::has_feasibly_castable_spell(runner.state(), P0, None),
            "precondition: the {{G}} instant is feasibly castable"
        );
        assert!(
            !super::auto_pass_recommended(
                runner.state(),
                &super::flat_priority_actions(runner.state())
            ),
            "castable instant + mana on opponent's turn → hold"
        );

        // Own-turn control arm: HOLD via the own-turn castability rung (the
        // rung below the upkeep/draw fast path), proving the two castability
        // rungs are gated symmetrically on `active_player` (in)equality.
        runner.state_mut().active_player = P0;
        assert!(
            !super::auto_pass_recommended(
                runner.state(),
                &super::flat_priority_actions(runner.state())
            ),
            "castable instant on your own turn → hold via own-turn castability rung"
        );
    }

    #[test]
    fn auto_pass_holds_when_tapping_land_for_mana_would_queue_non_mana_trigger() {
        let mut state = setup_opponent_priority(Phase::PreCombatMain);
        let land = create_land(&mut state, "Forest", &[]);
        add_fixed_mana_ability(&mut state, land, ManaColor::Green);
        add_mana_trigger_source(
            &mut state,
            "Manabarbs-like Trigger",
            TriggerMode::TapsForMana,
            draw_controller_effect(),
        );

        let flat = super::flat_priority_actions(&state);
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "a non-mana TapsForMana trigger makes tapping the land a meaningful priority action"
        );
    }

    #[test]
    fn auto_pass_skips_pure_triggered_mana_bonus_when_no_mana_is_spendable() {
        let mut state = setup_opponent_priority(Phase::PreCombatMain);
        let land = create_land(&mut state, "Forest", &[]);
        add_fixed_mana_ability(&mut state, land, ManaColor::Green);
        add_mana_trigger_source(
            &mut state,
            "Wild Growth-like Trigger",
            TriggerMode::TapsForMana,
            add_green_mana_effect(),
        );

        let flat = super::flat_priority_actions(&state);
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "pure triggered mana should not stop auto-pass when the mana is unusable"
        );
    }

    #[test]
    fn auto_pass_classifies_mana_added_triggers_by_triggered_mana_rules() {
        let mut mana_only = setup_opponent_priority(Phase::PreCombatMain);
        let land = create_land(&mut mana_only, "Forest", &[]);
        add_fixed_mana_ability(&mut mana_only, land, ManaColor::Green);
        add_mana_trigger_source(
            &mut mana_only,
            "Mana Echo",
            TriggerMode::ManaAdded,
            add_green_mana_effect(),
        );
        let flat = super::flat_priority_actions(&mana_only);
        assert!(
            super::auto_pass_recommended(&mana_only, &flat),
            "all-mana ManaAdded triggers are triggered mana abilities and should not stop auto-pass"
        );

        let mut non_mana = setup_opponent_priority(Phase::PreCombatMain);
        let land = create_land(&mut non_mana, "Forest", &[]);
        add_fixed_mana_ability(&mut non_mana, land, ManaColor::Green);
        add_mana_trigger_source(
            &mut non_mana,
            "Mana Draw Trigger",
            TriggerMode::ManaAdded,
            draw_controller_effect(),
        );
        let flat = super::flat_priority_actions(&non_mana);
        assert!(
            !super::auto_pass_recommended(&non_mana, &flat),
            "non-mana ManaAdded triggers should stop auto-pass"
        );
    }

    /// Issue #4388 / #544 (narrowed): on an opponent's turn a BARE sac-for-mana
    /// ability (Krark-Clan Ironworks) with an empty hand and no downstream
    /// follow-up enables nothing — the frontend auto-pass recommendation fires.
    /// The classifier `has_meaningful_priority_action` still returns true (the
    /// loop firewall must keep counting the sacrifice as a board change, CR
    /// 605.3a + 603.6), so the two deliberately disagree for this bare case.
    #[test]
    fn auto_pass_fires_for_bare_sac_for_mana_opponents_turn() {
        use crate::game::zones::create_object;
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(1);
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
            // Same BARE Sacrifice + Typed(Artifact) cost as the #544 test —
            // drives the real meaningful-priority classifier.
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

        // Production path: the flat `legal_actions` list omits sac-for-mana
        // abilities (they live in `legal_actions_by_object` only, #544), so both
        // the meaningful-priority classifier and the auto-pass gate must reach
        // the KCI activation through the internal `activatable_object_mana_actions`
        // scan — not through a caller-injected activation in the passed vec.
        let (flat, _, grouped) = legal_actions_full(&state);
        assert!(
            !flat.iter().any(|a| matches!(
                a,
                GameAction::ActivateAbility { source_id, .. } if *source_id == kci
            )),
            "precondition: KCI activation is grouped-only during priority, so the \
             hold must flow through the internal sac-for-mana scan"
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
        // Reach-guard (UNCHANGED): even with the sac-for-mana omitted from the
        // flat list, the classifier still returns true via its internal-scan
        // branch, so this test truly reaches the auto-pass sac rung and does not
        // pass vacuously (CR 605.3a + 603.6).
        assert!(
            super::has_meaningful_priority_action(&state, &flat),
            "precondition: sac-for-mana is a meaningful priority decision reached via the internal scan"
        );
        // NARROWED (this task): bare sac-for-mana on the opponent's turn with an
        // empty hand and no downstream follow-up → auto-pass FIRES. Flipped
        // assertion — fails if the narrowing is reverted.
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "bare sac-for-mana on opponent's turn with no downstream follow-up → auto-pass fires; \
             classifier stays meaningful for the loop firewall, so the two deliberately disagree"
        );
    }

    /// Sacrifice-for-mana source (KCI-proven cost shape): an artifact whose only
    /// ability is a mana ability whose cost sacrifices an artifact — self-
    /// satisfiable, so a lone source classifies as `ManaSourcePenalty::Sacrifices`
    /// and is activatable. Colorless mana models the Eldrazi-Spawn/altar class.
    fn add_sac_for_mana_source(state: &mut GameState, controller: PlayerId) -> ObjectId {
        use crate::types::ability::{TypeFilter, TypedFilter};
        let id = create_object(
            state,
            CardId(700),
            controller,
            "Sacrifice-for-Mana Altar".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
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
        id
    }

    /// V1 (repro fix): on an OPPONENT's turn, a lone sacrifice-for-mana source
    /// with nothing castable in hand and an opponent trigger on the stack must
    /// recommend auto-pass — the sacrifice/its mana enables no concrete follow-up
    /// (CR 605.3a + CR 603.6 + CR 117.1d). Before the fix `auto_pass_recommended`
    /// returned `false` (unconditional sac hold), forcing a pointless stop.
    #[test]
    fn auto_pass_fires_for_bare_sac_for_mana_on_opponents_turn_repro() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // P0 controls the lone sac-for-mana source; P0's hand is empty.
        add_sac_for_mana_source(&mut state, PlayerId(0));

        // Opponent (P1) spell on top of the stack — its controller != P0, so the
        // own-stack auto-pass shortcut (`top.controller == player`) cannot be the
        // path that fires. This isolates the narrowed sac rung as the cause.
        let opp_spell = create_object(
            &mut state,
            CardId(900),
            PlayerId(1),
            "Opponent Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: opp_spell,
            source_id: opp_spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(900),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let flat = super::flat_priority_actions(&state);

        // Reach-guards (non-vacuous): the sac source is genuinely activatable AND
        // the classifier counts it meaningful, so we truly reach the sac rung.
        assert!(
            super::has_activatable_sacrifice_for_mana(&state),
            "precondition: the sac-for-mana ability is activatable"
        );
        assert!(
            super::has_meaningful_priority_action(&state, &flat),
            "precondition: the classifier reaches the sac rung (loop firewall intact)"
        );
        // Sibling guard: the top of stack belongs to the opponent.
        assert_ne!(
            state.stack.back().unwrap().controller,
            PlayerId(0),
            "top of stack is the opponent's, so the own-stack shortcut is not under test"
        );
        // Nothing castable — the produced mana would feed no spell (case 1 false).
        assert!(
            !super::has_feasibly_castable_spell(&state, PlayerId(0), None),
            "precondition: empty hand, nothing feasibly castable"
        );

        // Flipped-on-revert assertion: fails if the narrowing is reverted.
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "bare sac-for-mana on opponent's turn with no downstream follow-up → auto-pass fires"
        );
    }

    /// V3 (#544 case 1 preserved): a sac-for-mana source PLUS a feasibly castable
    /// instant on an opponent's turn holds — the mana can pay the spell (CR 117.1d
    /// + CR 601.2g). Hold flows through the castability rung above the sac rung.
    #[test]
    fn sac_for_mana_holds_with_downstream_castable_spell() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test Bolt", true, "Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            });
        let mut runner = scenario.build();
        add_sac_for_mana_source(runner.state_mut(), P0);
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }

        // Positive reach-guard: the {G} instant is genuinely castable, so the HOLD
        // cannot pass vacuously.
        assert!(
            super::has_feasibly_castable_spell(runner.state(), P0, None),
            "precondition: the {{G}} instant is feasibly castable"
        );
        assert!(
            !super::auto_pass_recommended(
                runner.state(),
                &super::flat_priority_actions(runner.state())
            ),
            "sac-for-mana + castable spell → hold (case 1 via castability rung)"
        );

        // Negative sibling — same board minus the hand spell: a lone sac-for-mana
        // with nothing castable auto-passes.
        let mut bare = GameScenario::new();
        bare.at_phase(Phase::PreCombatMain);
        bare.add_basic_land(P0, ManaColor::Green);
        let mut bare_runner = bare.build();
        add_sac_for_mana_source(bare_runner.state_mut(), P0);
        {
            let state = bare_runner.state_mut();
            state.active_player = P1;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        assert!(
            !super::has_feasibly_castable_spell(bare_runner.state(), P0, None),
            "precondition (negative): no castable spell on the bare board"
        );
        assert!(
            super::auto_pass_recommended(
                bare_runner.state(),
                &super::flat_priority_actions(bare_runner.state())
            ),
            "negative sibling: sac-for-mana with nothing castable → auto-pass fires"
        );
    }

    /// V4 (#544 case 2): a sac-for-mana source PLUS a mana-costed non-mana
    /// activated ability holds — the produced mana could feed the ability
    /// (CR 605.3a + CR 603.6). The ability is present but unaffordable-now (no
    /// floating mana), so it is absent from the flat list — proving the hold
    /// flows through the case-2 followup gate, not the flat meaningful-action path.
    #[test]
    fn sac_for_mana_holds_with_downstream_mana_costed_activated_ability() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        add_sac_for_mana_source(&mut state, PlayerId(0));

        // Separate permanent with "{1}: Draw a card" — a non-mana activated
        // ability with a mana component in its cost.
        let engine = create_object(
            &mut state,
            CardId(710),
            PlayerId(0),
            "Mana-Costed Engine".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&engine).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                }),
            );
        }

        let flat = super::flat_priority_actions(&state);
        // Reach-guard: the flat list carries no meaningful action (the {1} ability
        // is unaffordable now), so the hold MUST come from the case-2 gate.
        assert!(
            !super::flat_actions_have_meaningful_priority(&state, &flat),
            "precondition: the mana-costed ability is unaffordable-now, absent from the flat list"
        );
        assert!(
            super::has_activatable_sacrifice_for_mana(&state),
            "precondition: the sac-for-mana source is activatable (we reach the sac rung)"
        );
        assert!(
            super::sacrifice_for_mana_enables_followup(&state, PlayerId(0)),
            "precondition: case 2 fires on the mana-costed activated ability"
        );
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "sac-for-mana + mana-costed activated ability → hold (case 2)"
        );

        // Negative sibling: replace the mana cost with a bare {T} cost (no mana
        // component) and tap the permanent so it stays out of the flat list. Case
        // 2 requires a mana-consuming cost, so it no longer fires → auto-pass.
        {
            let obj = state.objects.get_mut(&engine).unwrap();
            obj.tapped = true;
            Arc::make_mut(&mut obj.abilities).clear();
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
        let flat = super::flat_priority_actions(&state);
        assert!(
            !super::flat_actions_have_meaningful_priority(&state, &flat),
            "precondition (negative): the tapped {{T}} ability is absent from the flat list"
        );
        assert!(
            !super::sacrifice_for_mana_enables_followup(&state, PlayerId(0)),
            "negative: a non-mana ({{T}}) cost does not satisfy case 2"
        );
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "negative sibling: no mana-component ability → case 2 fails → auto-pass fires"
        );
    }

    /// V5 (#544 case 3, aristocrat): a sac-for-mana source PLUS a
    /// leaves-the-battlefield trigger on a SEPARATE controlled permanent (Blood
    /// Artist shape: `ChangesZone` origin battlefield → graveyard) holds — the
    /// sacrifice is itself the payoff (CR 603.6c). Multi-authority: the trigger
    /// lives on a different permanent, proving the scan sweeps all permanents.
    #[test]
    fn sac_for_mana_holds_with_beneficial_death_trigger() {
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let sac = add_sac_for_mana_source(&mut state, PlayerId(0));

        // Separate Blood-Artist-shaped permanent: "whenever a creature dies" =
        // ChangesZone from battlefield to graveyard.
        let blood_artist = create_object(
            &mut state,
            CardId(720),
            PlayerId(0),
            "Blood Artist".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&blood_artist).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .origin(Zone::Battlefield)
                    .destination(Zone::Graveyard),
            );
        }

        let flat = super::flat_priority_actions(&state);
        assert_ne!(
            sac, blood_artist,
            "the trigger lives on a separate permanent from the sac source"
        );
        assert!(
            !super::flat_actions_have_meaningful_priority(&state, &flat),
            "precondition: no flat meaningful action; the hold must come from case 3"
        );
        assert!(
            super::has_activatable_sacrifice_for_mana(&state),
            "precondition: the sac-for-mana source is activatable"
        );
        assert!(
            super::sacrifice_for_mana_enables_followup(&state, PlayerId(0)),
            "precondition: case 3 fires on the separate leaves-battlefield trigger"
        );
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "sac-for-mana + leaves-battlefield trigger → hold (case 3)"
        );
    }

    /// V5b (disjunctive-clause LTB): a Syr-Konrad-shaped trigger whose
    /// leaves-the-battlefield event lives ONLY in a `zone_change_clauses` clause
    /// (not the scalar `origin`/`origin_zones`) must ALSO hold — the case-3 scan
    /// consults the clause origins. Preserves the "always errs toward holding"
    /// invariant for disjunctive dies/leaves triggers.
    #[test]
    fn sac_for_mana_holds_with_disjunctive_leaves_battlefield_clause_trigger() {
        use crate::types::ability::{OriginConstraint, ZoneChangeClause};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        add_sac_for_mana_source(&mut state, PlayerId(0));

        let konrad = create_object(
            &mut state,
            CardId(730),
            PlayerId(0),
            "Syr Konrad, the Grim".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&konrad).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Disjunctive trigger: scalar origin is None; the battlefield-exit
            // event lives only in a clause.
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone).zone_change_clauses(vec![
                    ZoneChangeClause {
                        origin: OriginConstraint::Equals(Zone::Battlefield),
                        destination: None,
                        destination_constraint: OriginConstraint::Any,
                        valid_card: None,
                    },
                    ZoneChangeClause {
                        origin: OriginConstraint::NotEquals(Zone::Battlefield),
                        destination: Some(Zone::Graveyard),
                        destination_constraint: OriginConstraint::Any,
                        valid_card: None,
                    },
                ]),
            );
        }

        let flat = super::flat_priority_actions(&state);
        assert!(
            super::sacrifice_for_mana_enables_followup(&state, PlayerId(0)),
            "precondition: case 3 fires on the disjunctive battlefield-exit clause"
        );
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "sac-for-mana + disjunctive leaves-battlefield clause trigger → hold (case 3)"
        );
    }

    /// V6 (case 3 negative): a sac-for-mana source PLUS only an enters-battlefield
    /// / spell-cast trigger enables nothing when the permanent is sacrificed, so
    /// auto-pass fires. Boundary: `ChangesZone` with a non-battlefield origin and
    /// `SpellCast` must NOT match `trigger_fires_on_leaving_battlefield`.
    #[test]
    fn sac_for_mana_auto_passes_with_only_non_leaving_trigger() {
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        add_sac_for_mana_source(&mut state, PlayerId(0));

        let etb = create_object(
            &mut state,
            CardId(740),
            PlayerId(0),
            "Enters-Trigger Permanent".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&etb).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Enters-the-battlefield: ChangesZone from library → battlefield.
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .origin(Zone::Library)
                    .destination(Zone::Battlefield),
            );
            // A spell-cast trigger is likewise not a battlefield-exit payoff.
            obj.trigger_definitions
                .push(TriggerDefinition::new(TriggerMode::SpellCast));
        }

        let flat = super::flat_priority_actions(&state);
        assert!(
            super::has_activatable_sacrifice_for_mana(&state),
            "precondition: the sac-for-mana source is activatable (we reach the sac rung)"
        );
        assert!(
            !super::sacrifice_for_mana_enables_followup(&state, PlayerId(0)),
            "precondition: neither the enters nor the spell-cast trigger satisfies case 3"
        );
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "sac-for-mana with only non-leaving triggers → auto-pass fires"
        );
    }

    // ── G1: beneficial mana-tap trigger hold (CR 605.1b) ────────────────────

    /// A `{T}: Add {G}` mana creature (Zhur-Taa Druid shape). `trigger`, when
    /// given, is attached as the TapsForMana beneficial trigger. The creature is
    /// not summoning-sick so its tap ability is genuinely activatable and appears
    /// in `activatable_object_mana_actions`.
    fn add_mana_tap_creature(
        state: &mut GameState,
        controller: PlayerId,
        trigger: Option<TriggerDefinition>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(810),
            controller,
            "Mana Dork".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.summoning_sick = false;
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
        if let Some(trigger) = trigger {
            obj.trigger_definitions.push(trigger);
        }
        id
    }

    /// Zhur-Taa Druid's live trigger: `Whenever you tap ~ for mana, it deals 1
    /// damage to each opponent.` (`valid_card: SelfRef`, `valid_target:
    /// Controller`, execute `DamageEachPlayer { Opponent }`).
    fn zhur_taa_trigger() -> TriggerDefinition {
        use crate::types::ability::PlayerFilter;
        use crate::types::triggers::TriggerMode;
        TriggerDefinition::new(TriggerMode::TapsForMana)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DamageEachPlayer {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player_filter: PlayerFilter::Opponent,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .valid_target(TargetFilter::Controller)
    }

    fn own_main_priority() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    /// False-pass POSITIVE (revert-failing): Zhur-Taa Druid on own PreCombatMain,
    /// empty stack, nothing else to do → HOLD. If the G1 rung is reverted, rung 9
    /// finds no meaningful action (a bare mana ability) and auto-passes, so the
    /// `!auto_pass_recommended` assertion flips.
    #[test]
    fn g1_holds_for_beneficial_mana_tap_trigger_own_main() {
        let mut state = own_main_priority();
        add_mana_tap_creature(&mut state, PlayerId(0), Some(zhur_taa_trigger()));
        let flat = super::flat_priority_actions(&state);

        // Reach-guards (non-vacuous): stage-1 finds the beneficial source AND the
        // tap ability is genuinely activatable (so stage-2 is reached), yet the
        // flat list is NOT meaningful (a bare mana ability), so a HOLD here can
        // only come from the G1 rung.
        assert!(
            !mana_sources::beneficial_non_mana_tap_trigger_sources(&state, PlayerId(0)).is_empty(),
            "precondition: stage-1 AST gate finds the beneficial trigger source"
        );
        assert!(
            !super::activatable_object_mana_actions(&state).is_empty(),
            "precondition: the {{T}} mana ability is activatable (stage-2 reached)"
        );
        assert!(
            !super::flat_actions_have_meaningful_priority(&state, &flat),
            "precondition: the flat list is not meaningful (hold is not from rung 9)"
        );

        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "beneficial mana-tap trigger on own main → HOLD (G1 rung)"
        );
    }

    /// NEGATIVE — Manabarbs shape (`DealDamage { TriggeringPlayer }`): the effect
    /// harms the tapper, not an opponent, so the sign classifier rejects it and
    /// auto-pass fires. Reach-guard: the tap ability is still activatable (so the
    /// pass is due to the sign classifier, not an unreachable stage 2).
    #[test]
    fn g1_auto_passes_for_manabarbs_shape() {
        use crate::types::triggers::TriggerMode;
        let mut state = own_main_priority();
        let manabarbs = TriggerDefinition::new(TriggerMode::TapsForMana)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::TriggeringPlayer,
                    damage_source: None,
                    excess: None,
                },
            ))
            .valid_card(TargetFilter::Typed(TypedFilter::land()));
        add_mana_tap_creature(&mut state, PlayerId(0), Some(manabarbs));
        let flat = super::flat_priority_actions(&state);

        assert!(
            !super::activatable_object_mana_actions(&state).is_empty(),
            "reach-guard: the tap ability is activatable; only the sign rejects it"
        );
        assert!(
            mana_sources::beneficial_non_mana_tap_trigger_sources(&state, PlayerId(0)).is_empty(),
            "the harm-the-tapper effect is not classified beneficial"
        );
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "Manabarbs-shaped self-harm trigger → auto-pass"
        );
    }

    /// Forbidden Orchard token (`Effect::Token` owner Opponent): not a CR
    /// 119/120 life/damage swing, so G1's beneficial sign classifier rejects it,
    /// but the grouped-mana probe still holds because tapping for mana queues a
    /// non-mana token trigger.
    #[test]
    fn auto_pass_holds_for_forbidden_orchard_token_via_grouped_mana() {
        use crate::types::ability::PtValue;
        use crate::types::triggers::TriggerMode;
        let mut state = own_main_priority();
        // Forbidden Orchard's live shape: "target opponent creates a 1/1 Spirit".
        let orchard = TriggerDefinition::new(TriggerMode::TapsForMana)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Token {
                    name: "Spirit".to_string(),
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    types: vec!["Creature".to_string(), "Spirit".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![],
                        controller: Some(ControllerRef::Opponent),
                        properties: vec![],
                    }),
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            ))
            .valid_card(TargetFilter::SelfRef);
        add_mana_tap_creature(&mut state, PlayerId(0), Some(orchard));
        let flat = super::flat_priority_actions(&state);

        assert!(
            !super::activatable_object_mana_actions(&state).is_empty(),
            "reach-guard: the tap ability is activatable; only the sign rejects it"
        );
        assert!(
            mana_sources::beneficial_non_mana_tap_trigger_sources(&state, PlayerId(0)).is_empty(),
            "a token-creation rider is not a CR 119/120 benefit"
        );
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "Forbidden-Orchard token rider queues a non-mana trigger → hold"
        );
    }

    /// NEGATIVE — opponent-controlled trigger source (also covers "trigger on the
    /// opponent's battlefield"): the beneficial dork is P1's, so on P0's turn
    /// `beneficial_non_mana_tap_trigger_sources(P0)` is empty and P1's source is
    /// absent from P0's mana sweep → auto-pass. Reach-guard: the SAME dork
    /// controlled by P0 would hold (verified by the positive test).
    #[test]
    fn g1_auto_passes_for_opponent_controlled_source() {
        let mut state = own_main_priority();
        add_mana_tap_creature(&mut state, PlayerId(1), Some(zhur_taa_trigger()));
        let flat = super::flat_priority_actions(&state);

        assert!(
            mana_sources::beneficial_non_mana_tap_trigger_sources(&state, PlayerId(0)).is_empty(),
            "an opponent-controlled beneficial source is not P0's"
        );
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "opponent-controlled beneficial trigger source → auto-pass"
        );
    }

    /// NEGATIVE — the beneficial dork is already tapped, so its tap ability is
    /// absent from the mana sweep: stage 1 still finds the source, but stage 2
    /// finds no activatable matching mana action → auto-pass. Reach-guard:
    /// stage-1 non-empty (so the pass is a stage-2 miss, not a stage-1 miss).
    #[test]
    fn g1_auto_passes_when_source_already_tapped() {
        let mut state = own_main_priority();
        let dork = add_mana_tap_creature(&mut state, PlayerId(0), Some(zhur_taa_trigger()));
        state.objects.get_mut(&dork).unwrap().tapped = true;
        let flat = super::flat_priority_actions(&state);

        assert!(
            !mana_sources::beneficial_non_mana_tap_trigger_sources(&state, PlayerId(0)).is_empty(),
            "reach-guard: stage-1 still finds the beneficial source"
        );
        assert!(
            super::activatable_object_mana_actions(&state).is_empty(),
            "the tapped source has no activatable mana action (stage-2 miss)"
        );
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "tapped beneficial source → auto-pass (self-quiescing)"
        );
    }

    /// NEGATIVE — wrong window (own Upkeep, not a main phase): the G1 window gate
    /// requires PreCombatMain/PostCombatMain, so on own Upkeep the beneficial hold
    /// does not apply. Reach-guard: on the SAME board at PreCombatMain the hold
    /// fires (asserted first), proving only the phase axis flips the result.
    #[test]
    fn g1_auto_passes_on_own_upkeep_wrong_phase() {
        let mut state = own_main_priority();
        add_mana_tap_creature(&mut state, PlayerId(0), Some(zhur_taa_trigger()));
        let flat = super::flat_priority_actions(&state);

        // Reach-guard: at a main phase this exact board HOLDS.
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "reach-guard: the same board holds at PreCombatMain"
        );

        // Flip only the phase to Upkeep — G1's window no longer applies.
        state.phase = Phase::Upkeep;
        assert!(
            super::auto_pass_recommended(&state, &flat),
            "own Upkeep is outside G1's mains-only window → auto-pass"
        );
    }

    /// Opponent's turn: G1 requires `active_player == player`, but the broader
    /// grouped-mana probe still holds because tapping the source for mana queues
    /// a non-mana tap trigger. Reach-guard: the same board on P0's turn holds.
    #[test]
    fn auto_pass_holds_for_mana_tap_trigger_on_opponents_turn_via_grouped_mana() {
        let mut state = own_main_priority();
        add_mana_tap_creature(&mut state, PlayerId(0), Some(zhur_taa_trigger()));
        let flat = super::flat_priority_actions(&state);

        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "reach-guard: on P0's own turn the board holds"
        );

        // Flip only the turn — it is now the opponent's (P1's) turn.
        state.active_player = PlayerId(1);
        assert!(
            !super::auto_pass_recommended(&state, &flat),
            "opponent's turn is outside G1, but grouped mana still sees the queued non-mana trigger → hold"
        );
    }

    /// V6b (predicate boundary): direct unit coverage of
    /// `trigger_fires_on_leaving_battlefield`, pinning the `OriginConstraint`
    /// semantics for the disjunctive-clause arm (CR 603.6c).
    #[test]
    fn trigger_fires_on_leaving_battlefield_predicate_boundary() {
        use crate::types::ability::{OriginConstraint, ZoneChangeClause};
        use crate::types::triggers::TriggerMode;

        // Direct battlefield-exit modes always fire.
        for mode in [
            TriggerMode::LeavesBattlefield,
            TriggerMode::Sacrificed,
            TriggerMode::SacrificedOnce,
            TriggerMode::Destroyed,
        ] {
            assert!(
                super::trigger_fires_on_leaving_battlefield(&TriggerDefinition::new(mode.clone())),
                "{mode:?} is a battlefield-exit event"
            );
        }
        // ChangesZone with a scalar battlefield origin fires.
        assert!(super::trigger_fires_on_leaving_battlefield(
            &TriggerDefinition::new(TriggerMode::ChangesZone).origin(Zone::Battlefield)
        ));
        // Disjunctive clause whose origin includes the battlefield fires.
        assert!(super::trigger_fires_on_leaving_battlefield(
            &TriggerDefinition::new(TriggerMode::ChangesZone).zone_change_clauses(vec![
                ZoneChangeClause {
                    origin: OriginConstraint::Equals(Zone::Battlefield),
                    destination: None,
                    destination_constraint: OriginConstraint::Any,
                    valid_card: None,
                },
            ])
        ));
        // "from anywhere other than the battlefield" does NOT fire.
        assert!(!super::trigger_fires_on_leaving_battlefield(
            &TriggerDefinition::new(TriggerMode::ChangesZone).zone_change_clauses(vec![
                ZoneChangeClause {
                    origin: OriginConstraint::NotEquals(Zone::Battlefield),
                    destination: Some(Zone::Graveyard),
                    destination_constraint: OriginConstraint::Any,
                    valid_card: None,
                },
            ])
        ));
        // Enters-battlefield (origin Library) and SpellCast do NOT fire.
        assert!(!super::trigger_fires_on_leaving_battlefield(
            &TriggerDefinition::new(TriggerMode::ChangesZone).origin(Zone::Library)
        ));
        assert!(!super::trigger_fires_on_leaving_battlefield(
            &TriggerDefinition::new(TriggerMode::SpellCast)
        ));
    }

    /// V7 (Mana Leak / Daze non-interference): `auto_pass_recommended` must never
    /// suppress a non-`Priority` decision prompt. A "counter unless you pay {1}"
    /// surfaces as `WaitingFor::UnlessPayment` (CR 118.12a), and even with a
    /// sac-for-mana `{C}` source on the payer's board the recommendation returns
    /// `false` — the guard that early-returns for non-`Priority` states
    /// (mod.rs `_ => return false`) makes this safe.
    #[test]
    fn unless_pay_surfaces_pay_cost_prompt_despite_sac_for_mana_autopass() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);

        let sac = add_sac_for_mana_source(&mut state, PlayerId(0));

        // Mana-Leak-shaped prompt: P0 must pay {1} or the effect happens.
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            pending_effect: Box::new(empty_effect(sac)),
            trigger_event: None,
            effect_description: Some("counter target spell".to_string()),
            remaining: vec![],
        };

        // Reach-guard: the waiting state is genuinely non-`Priority`.
        assert!(
            matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
            "precondition: the prompt is UnlessPayment (PayCost class), not Priority"
        );
        // Non-interference: auto-pass never fires for a non-`Priority` prompt.
        assert!(
            !super::auto_pass_recommended(&state, &[]),
            "auto-pass must not suppress an unless-pay prompt despite a sac-for-mana source"
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
                source_controller: None,
                bypass_beneficiary: None,
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
            persist_player: None,
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
                phase: MulliganDecisionPhase::Declare,
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
                chooser: None,
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
                chooser: None,
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
        let counters = crate::game::perf_counters::snapshot();

        assert_eq!(counters.state_clone_for_legality, 0);
        assert_eq!(counters.priority_cast_probe_builds, 0);
        assert_eq!(actions.len(), 26);
        assert!(spell_costs.is_empty());
        assert!(grouped.is_empty());
        assert!(actions
            .iter()
            .any(|action| matches!(action, GameAction::ChooseTarget { target: None })));
    }

    #[test]
    fn legal_actions_priority_cast_probe_reuses_one_flushed_state_and_one_auto_tap_cache() {
        use crate::types::mana::ManaCostShard;
        use crate::types::phase::Phase;

        let mut state = setup_priority();
        state.phase = Phase::PreCombatMain;

        for i in 0..3 {
            create_land(&mut state, &format!("Island {i}"), &["Island"]);
        }

        let mut spell_ids = Vec::new();
        for i in 0..3 {
            let spell = create_object(
                &mut state,
                CardId(4000 + i),
                PlayerId(0),
                format!("Blue Spell {i}"),
                Zone::Hand,
            );
            {
                let obj = state.objects.get_mut(&spell).unwrap();
                obj.card_types.core_types.push(CoreType::Sorcery);
                obj.mana_cost = ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 0,
                };
                Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ));
            }
            spell_ids.push(spell);
        }

        crate::game::perf_counters::reset();
        let (actions, _spell_costs, _grouped) = legal_actions_full(&state);
        let counters = crate::game::perf_counters::snapshot();

        for spell in &spell_ids {
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if object_id == spell)),
                "priority legal actions must include each castable spell"
            );
        }
        assert_eq!(counters.priority_cast_probe_builds, 1);
        assert_eq!(counters.auto_tap_source_cache_builds, 1);
        assert!(
            counters.cached_auto_tap_source_reuses >= spell_ids.len() as u64,
            "expected at least one cached auto-tap source reuse per cast probe, got {:?}",
            counters
        );
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
                chooser: None,
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
    /// bottom-cards flows (a `MulliganDecision` entry mid-`BottomCards`, and
    /// `OpeningHandBottomCards`) always offer each pending player a
    /// `SelectCards` action, so the detector must not fire. The pending player
    /// is given enough hand cards to satisfy the owed bottom `count`.
    #[test]
    fn stuck_diagnostic_none_on_normal_bottom_cards() {
        use crate::types::game_state::{MulliganBottomEntry, OpeningHandBottomReason};

        for waiting_for in [
            WaitingFor::MulliganDecision {
                pending: vec![MulliganDecisionEntry {
                    player: PlayerId(0),
                    mulligan_count: 1,
                    phase: MulliganDecisionPhase::BottomCards {
                        count: 1,
                        then: PendingMulliganAction::Keep,
                    },
                }],
                free_first_mulligan: false,
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

    /// Issue #4878: `validated_candidate_actions` must return candidates in the
    /// canonical `GameAction::cmp_stable` order, independent of the upstream
    /// enumeration order (which walks `state.objects`, an `im::HashMap`, in
    /// hash order — not sorted for this id set). Reverting the
    /// `sort_by(cmp_stable)` guard returns the candidates in that hash order,
    /// which differs from the canonical order below, flipping this assertion.
    #[test]
    fn validated_candidates_are_cmp_stable_sorted() {
        let mut state = setup_priority();
        // CR 305.2 + CR 505: land drop available in the precombat main phase so
        // each land in hand survives the simulation filter as a `PlayLand`.
        state.phase = Phase::PreCombatMain;
        state.lands_played_this_turn = 0;
        for _ in 0..10 {
            let id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Hand,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }

        let actions: Vec<GameAction> = validated_candidate_actions(&state)
            .iter()
            .map(|c| c.action.clone())
            .collect();

        // Reach guard: the scenario must actually offer several distinct actions
        // (multiple `PlayLand` + `PassPriority`), otherwise a single-element list
        // would be trivially "sorted" and the assertion vacuous.
        assert!(
            actions.len() >= 3,
            "expected several candidate actions to canonicalize, got {}",
            actions.len()
        );

        // Canonical order: identical to an independent `cmp_stable` sort.
        let mut expected = actions.clone();
        expected.sort_by(|a, b| a.cmp_stable(b));
        assert_eq!(
            actions, expected,
            "validated_candidate_actions must emit cmp_stable-canonical order"
        );

        // Determinism: a second call over the same state yields the same order.
        let again: Vec<GameAction> = validated_candidate_actions(&state)
            .iter()
            .map(|c| c.action.clone())
            .collect();
        assert_eq!(actions, again, "candidate order must be deterministic");
    }

    /// CR 117.3d: a matching priority yield for the top-of-stack trigger makes
    /// `auto_pass_recommended` return `true`, overriding the meaningful-action
    /// hold that would otherwise keep the window open. Reverting the yield short-
    /// circuit flips this back to `false`.
    #[test]
    fn auto_pass_recommended_true_for_yielded_top() {
        let mut state = setup_priority();
        // Opponent-controlled token trigger on top of the stack.
        let source = ObjectId(500);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(1),
        );
        ability.source_incarnation = Some(2);
        ability.source_card_id = Some(CardId(77));
        state.stack.push_back(StackEntry {
            id: ObjectId(600),
            source_id: source,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Token".to_string(),
                subject_match_count: None,
                die_result: None,
            },
        });
        // A meaningful action keeps auto-pass OFF absent a yield (reach-guard:
        // proves the yield is what flips the result).
        let actions = vec![GameAction::PlayLand {
            object_id: ObjectId(700),
            card_id: CardId(1),
        }];
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "without a yield, a meaningful action must keep the window open"
        );

        state.add_priority_yield(
            PlayerId(0),
            crate::types::game_state::YieldTarget::AllCopies {
                card_id: CardId(77),
                trigger_description: None,
            },
        );
        assert!(
            super::auto_pass_recommended(&state, &actions),
            "CR 117.3d: a matching yield overrides the meaningful-action hold"
        );
    }

    /// State-shape tests for the engine-owned phase-stop gate migrated out of the
    /// frontend (Step 5). `auto_passes_initial_priority_by_default` returns true on
    /// an own-turn Upkeep with an empty stack, which is the downstream `true` these
    /// tests reach absent the phase-stop gate — so the gate's `false` is never
    /// vacuous.
    #[test]
    fn auto_pass_recommended_false_on_empty_stack_phase_stop() {
        let mut state = setup_priority();
        state.phase = Phase::Upkeep;
        let actions = vec![GameAction::PassPriority];

        // Reach-guard: without any stop, the own-turn upkeep window auto-passes.
        assert!(
            super::auto_pass_recommended(&state, &actions),
            "reach-guard: empty-stack own-turn upkeep recommends auto-pass absent a stop"
        );

        // With an AllTurns stop on the current phase, the new gate refuses.
        state.phase_stops.insert(
            PlayerId(0),
            vec![PhaseStop {
                phase: Phase::Upkeep,
                scope: PhaseStopScope::AllTurns,
            }],
        );
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "empty stack + phase stop on the current phase must never auto-pass"
        );
    }

    #[test]
    fn auto_pass_recommended_true_when_phase_not_stopped() {
        let mut state = setup_priority();
        state.phase = Phase::Upkeep;
        // Stop configured on a DIFFERENT phase → this branch does not gate.
        state.phase_stops.insert(
            PlayerId(0),
            vec![PhaseStop {
                phase: Phase::End,
                scope: PhaseStopScope::AllTurns,
            }],
        );
        assert!(
            super::auto_pass_recommended(&state, &[GameAction::PassPriority]),
            "a stop on an unrelated phase must not gate the current phase"
        );
    }

    #[test]
    fn auto_pass_recommended_true_when_stack_nonempty_despite_stop() {
        let mut state = setup_priority();
        state.phase = Phase::Upkeep;
        // A phase stop on the current phase, but the stack is NON-empty: the new
        // gate is empty-stack-only (disjoint from the CR 117.3d yield short-circuit
        // below), so it must not fire here. Reverting the `is_empty()` guard flips
        // this to `false`.
        state.stack.push_back(StackEntry {
            id: ObjectId(600),
            source_id: ObjectId(600),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.phase_stops.insert(
            PlayerId(0),
            vec![PhaseStop {
                phase: Phase::Upkeep,
                scope: PhaseStopScope::AllTurns,
            }],
        );
        assert!(
            super::auto_pass_recommended(&state, &[GameAction::PassPriority]),
            "the empty-stack-only phase-stop gate must not fire while the stack is non-empty"
        );
    }

    #[test]
    fn auto_pass_recommended_phase_stop_per_player_isolation() {
        let mut state = setup_priority();
        state.phase = Phase::Upkeep;
        // Stop belongs to player 1; the priority seat is player 0 → the gate must
        // not fire for player 0.
        state.phase_stops.insert(
            PlayerId(1),
            vec![PhaseStop {
                phase: Phase::Upkeep,
                scope: PhaseStopScope::AllTurns,
            }],
        );
        assert!(
            super::auto_pass_recommended(&state, &[GameAction::PassPriority]),
            "player 1's phase stop must not gate player 0's auto-pass"
        );
    }

    /// Creates a battlefield permanent controlled by P0 carrying a single
    /// non-mana activated ability, so `ActivateAbility { source_id, 0 }` is a
    /// meaningful flat priority action for the auto-pass classifier.
    fn create_nonmana_activated_source(state: &mut GameState) -> ObjectId {
        let src = create_object(
            state,
            CardId(2),
            PlayerId(0),
            "Pinger".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&src).unwrap();
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Tap),
        );
        src
    }

    /// G3 (hoist): on your own turn, with your own spell on top of the stack AND
    /// a feasibly castable instant in hand, auto-pass fires — the hoisted own-top
    /// rung now outranks the own-turn castability HOLD. Before the hoist the
    /// rung-9 castability check returned `false` (HOLD); reverting the hoist flips
    /// this assertion back to `false`.
    #[test]
    fn auto_pass_true_for_own_spell_on_top_despite_castable_instant() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test Bolt", true, "Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            });

        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P0;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
            state.stack.push_back(StackEntry {
                id: ObjectId(900),
                source_id: ObjectId(901),
                controller: P0,
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }

        // Reach-guard: the {G} instant is genuinely castable, so absent the hoist
        // the own-turn castability rung would HOLD (return false). This is exactly
        // the false-HOLD the hoist fixes — the assertion below cannot pass
        // vacuously.
        assert!(
            super::has_feasibly_castable_spell(runner.state(), P0, None),
            "precondition: the {{G}} instant is feasibly castable — the castability rung would otherwise hold"
        );
        assert!(
            super::auto_pass_recommended(
                runner.state(),
                &super::flat_priority_actions(runner.state())
            ),
            "G3 hoist: own spell on top outranks the castability hold → auto-pass fires"
        );
    }

    /// G3 (MTGA parity): on an OPPONENT's turn, your own instant just cast and on
    /// top of the stack auto-passes even though you still hold another castable
    /// instant — the hoisted own-top rung outranks the opponent-turn castability
    /// HOLD. Reverting the hoist flips this to `false` (rung-7 castability holds).
    #[test]
    fn auto_pass_true_for_own_instant_on_top_opponents_turn() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test Bolt", true, "Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            });

        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
            // Your own instant already on the stack (controller P0).
            state.stack.push_back(StackEntry {
                id: ObjectId(900),
                source_id: ObjectId(901),
                controller: P0,
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }

        // Reach-guard: another castable instant remains in hand, so rung-7
        // opponent-turn castability WOULD hold absent the hoist.
        assert!(
            super::has_feasibly_castable_spell(runner.state(), P0, None),
            "precondition: a second {{G}} instant is feasibly castable"
        );
        assert!(
            super::auto_pass_recommended(
                runner.state(),
                &super::flat_priority_actions(runner.state())
            ),
            "G3 hoist (MTGA parity): own instant on top auto-passes even on the opponent's turn"
        );
    }

    /// G3 negative (reach-guarded): an OPPONENT's spell on top of the stack must
    /// still HOLD when you have a castable instant — the hoisted own-top rung only
    /// fires for objects YOU control, never for an opponent's. The castable-spell
    /// precondition proves the hold routes through the castability rung.
    #[test]
    fn auto_pass_false_for_opponent_spell_on_top_with_castable_instant() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::ManaCostShard;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test Bolt", true, "Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            });

        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
            // OPPONENT's spell on top (controller P1).
            state.stack.push_back(StackEntry {
                id: ObjectId(900),
                source_id: ObjectId(901),
                controller: P1,
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }

        // Reach-guard: the instant is feasibly castable, so the `false` below is
        // the castability HOLD firing — proving the hoisted own-top rung did NOT
        // fire for an opponent-controlled top-of-stack object.
        assert!(
            super::has_feasibly_castable_spell(runner.state(), P0, None),
            "precondition: the {{G}} instant is feasibly castable"
        );
        assert!(
            !super::auto_pass_recommended(
                runner.state(),
                &super::flat_priority_actions(runner.state())
            ),
            "opponent spell on top → still hold to respond (own-top rung must not fire for opponent objects)"
        );
    }

    /// G3 negative (reach-guarded): an opponent's spell on top plus a meaningful
    /// flat action HOLDS. The reach-guard proves that, absent the meaningful
    /// action, the same state auto-passes — so the hold is the meaningful action,
    /// and the hoisted own-top rung correctly did not fire for the opponent spell.
    #[test]
    fn auto_pass_false_for_opponent_spell_on_top_with_meaningful_action() {
        let mut state = setup_priority();
        state.phase = Phase::PreCombatMain;
        state.stack.push_back(StackEntry {
            id: ObjectId(600),
            source_id: ObjectId(601),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let src = create_nonmana_activated_source(&mut state);
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: src,
                ability_index: 0,
            },
        ];

        // Reach-guard: with only PassPriority the window auto-passes (the own-top
        // rung does not fire for the opponent's spell), so the hold below is
        // attributable to the meaningful action, not an upstream short-circuit.
        assert!(
            super::auto_pass_recommended(&state, &[GameAction::PassPriority]),
            "reach-guard: opponent spell on top with no meaningful action → auto-pass"
        );
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "opponent spell on top + meaningful action → hold"
        );
    }

    /// G3 (hoist, meaningful-ability variant): on your own turn with your own
    /// spell on top of the stack AND a meaningful non-mana activated ability
    /// available, auto-pass fires — the hoisted own-top rung (Rung 4) outranks
    /// the meaningful-action hold below (accepted MTGA parity). The reach-guard
    /// (same state/actions, EMPTY stack) proves the ability genuinely HOLDS the
    /// window absent an own-top entry, so the flip is non-vacuous: it is the
    /// presence of the own object on top that turns the hold into a pass. This
    /// covers the own-top rung for the meaningful-activated-ability input class
    /// (the existing `..._despite_castable_instant` / `..._own_instant_on_top_*`
    /// tests cover the rung-order-vs-castability delta); deleting or making the
    /// own-top rung unreachable for this input flips the main assertion to false.
    #[test]
    fn auto_pass_true_for_own_spell_on_top_despite_meaningful_ability() {
        let mut state = setup_priority();
        state.phase = Phase::PreCombatMain;
        let src = create_nonmana_activated_source(&mut state);
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: src,
                ability_index: 0,
            },
        ];

        // Reach-guard: with an EMPTY stack the meaningful activated ability
        // genuinely HOLDS the window (the meaningful-action hold) — auto-pass does
        // NOT fire. This proves the flip below is caused by the own-top entry, not
        // an upstream short-circuit.
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "reach-guard: own turn + meaningful activated ability, empty stack → hold"
        );

        // Own spell on top of the stack: the hoisted own-top rung (Rung 4) flips
        // that hold to a PASS, outranking the meaningful-action hold below.
        state.stack.push_back(StackEntry {
            id: ObjectId(900),
            source_id: ObjectId(901),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        assert!(
            super::auto_pass_recommended(&state, &actions),
            "G3 hoist: own spell on top outranks the meaningful-action hold → auto-pass fires"
        );
    }

    /// G2 (gate): a meaningful NON-cast flat action at your OWN upkeep now HOLDS
    /// instead of fast-passing, while a merely-castable instant keeps auto-passing
    /// (MTGA parity). Before the gate the bare
    /// `auto_passes_initial_priority_by_default` fast path returned `true`
    /// unconditionally; the `!flat_actions_have_meaningful_noncast_priority` gate
    /// flips the activated-ability case to `false`. The `CastSpell` control
    /// assertion proves the noncast predicate's `CastSpell => false` arm — it
    /// flips to `false` if the gate is (wrongly) switched to the broad
    /// `flat_actions_have_meaningful_priority` primitive.
    #[test]
    fn auto_pass_false_for_meaningful_noncast_action_on_own_upkeep() {
        let mut state = setup_priority();
        state.phase = Phase::Upkeep;
        let src = create_nonmana_activated_source(&mut state);
        let hold_actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: src,
                ability_index: 0,
            },
        ];
        let cast_actions = vec![
            GameAction::PassPriority,
            GameAction::CastSpell {
                object_id: ObjectId(10),
                card_id: CardId(10),
                targets: Vec::new(),
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
        ];

        // Reach-guard: absent any meaningful action this own-upkeep window
        // fast-passes through the gated rung — proving the window really is the
        // gated fast path and the meaningful action is what now holds it.
        assert!(
            super::auto_pass_recommended(&state, &[GameAction::PassPriority]),
            "reach-guard: empty own-upkeep window fast-passes via the gated rung"
        );
        // Control: a merely-castable instant does NOT hold the upkeep window
        // (MTGA parity) — the noncast predicate excludes `CastSpell`.
        assert!(
            super::auto_pass_recommended(&state, &cast_actions),
            "G2: a merely-castable instant at own upkeep keeps auto-passing (CastSpell excluded)"
        );
        // Gap fix: a meaningful non-mana activated ability HOLDS.
        assert!(
            !super::auto_pass_recommended(&state, &hold_actions),
            "G2: a meaningful non-cast action at own upkeep now HOLDS instead of fast-passing"
        );
    }

    /// G2 (gate, negative route-guard, REV3): a bare own upkeep whose only flat
    /// action is Pass — with a standalone (grouped-only) mana source on the
    /// battlefield — still fast-passes, and does so THROUGH the gated fast path.
    /// The route-guards prove both gate predicates hold and no meaningful non-cast
    /// action is present, so the `true` is the gate's.
    #[test]
    fn auto_pass_true_for_bare_own_upkeep_via_gated_fast_path() {
        let mut state = setup_priority();
        state.phase = Phase::Upkeep;
        // A standalone mana source: activatable mana, but grouped-only (NOT in the
        // flat priority list) — mirrors production.
        let land = create_land(&mut state, "Forest", &["Forest"]);
        add_fixed_mana_ability(&mut state, land, ManaColor::Green);
        let actions = vec![GameAction::PassPriority];

        // Route-guards: the gate window is live, the flat list carries no
        // meaningful non-cast action, and a standalone mana source IS activatable
        // (grouped-only). Together these prove the gated fast path is the rung
        // that returns true.
        assert!(
            super::auto_passes_initial_priority_by_default(&state),
            "route-guard: empty-stack own upkeep is the fast-path window"
        );
        assert!(
            !super::flat_actions_have_meaningful_noncast_priority(&state, &actions),
            "route-guard: the flat list carries no meaningful non-cast action"
        );
        assert!(
            !super::activatable_object_mana_actions(&state).is_empty(),
            "route-guard: a standalone mana source IS activatable (grouped-only)"
        );
        assert!(
            super::auto_pass_recommended(&state, &actions),
            "G2: bare own upkeep with only a standalone mana source fast-passes via the gate"
        );
    }

    /// G2 (active_player gate, REV2): a meaningful non-cast flat action on the
    /// OPPONENT's upkeep now HOLDS. The pre-Stage-2 bare fast path fast-passed any
    /// upkeep/draw window regardless of whose turn it was; gating on
    /// `active_player == player` flips this to `false`. The reach-guard proves the
    /// meaningful action is the discriminator.
    #[test]
    fn auto_pass_false_for_meaningful_action_on_opponents_upkeep() {
        let mut state = setup_priority();
        state.active_player = PlayerId(1);
        state.phase = Phase::Upkeep;
        let src = create_nonmana_activated_source(&mut state);
        let actions = vec![
            GameAction::PassPriority,
            GameAction::ActivateAbility {
                source_id: src,
                ability_index: 0,
            },
        ];

        // Reach-guard: with only PassPriority the opponent's upkeep window still
        // auto-passes, proving the meaningful action is what holds below.
        assert!(
            super::auto_pass_recommended(&state, &[GameAction::PassPriority]),
            "reach-guard: opponent's upkeep with no meaningful action → auto-pass"
        );
        assert!(
            !super::auto_pass_recommended(&state, &actions),
            "G2 active_player: a meaningful action on the OPPONENT's upkeep now HOLDS"
        );
    }
}
