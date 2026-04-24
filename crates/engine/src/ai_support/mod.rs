mod candidates;
mod context;
mod copy;
pub mod filter;

use std::collections::{HashMap, HashSet};

use crate::game::mana_abilities;
use crate::game::mana_sources;
use crate::types::ability::AbilityKind;
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;

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

fn cheap_reject_candidate(state: &GameState, action: &GameAction) -> bool {
    let Some(acting_player) = state.waiting_for.acting_player() else {
        return true;
    };

    match (&state.waiting_for, action) {
        (WaitingFor::Priority { player }, _) if *player != acting_player => true,
        (WaitingFor::Priority { .. }, GameAction::CastSpell { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::PlayLand { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::Transform { object_id })
        | (WaitingFor::Priority { .. }, GameAction::TurnFaceUp { object_id })
        | (WaitingFor::Priority { .. }, GameAction::PlayFaceDown { object_id, .. })
        | (WaitingFor::Priority { .. }, GameAction::TapLandForMana { object_id })
        | (WaitingFor::Priority { .. }, GameAction::UntapLandForMana { object_id }) => {
            !state.objects.contains_key(object_id)
        }
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
        (
            WaitingFor::CopyTargetChoice { valid_targets, .. },
            GameAction::ChooseTarget { target },
        ) => !matches_target_choice(target, valid_targets),
        (WaitingFor::ExploreChoice { choosable, .. }, GameAction::ChooseTarget { target }) => {
            !matches_target_choice(target, choosable)
        }
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
        (WaitingFor::LearnChoice { hand_cards, .. }, GameAction::LearnDecision { choice }) => {
            match choice {
                crate::types::actions::LearnOption::Rummage { card_id } => {
                    !hand_cards.contains(card_id) || !state.objects.contains_key(card_id)
                }
                crate::types::actions::LearnOption::Skip => false,
            }
        }
        (WaitingFor::DiscoverChoice { .. }, GameAction::DiscoverChoice { .. })
        | (WaitingFor::CascadeChoice { .. }, GameAction::CascadeChoice { .. })
        | (WaitingFor::MulliganDecision { .. }, GameAction::MulliganDecision { .. })
        | (WaitingFor::BetweenGamesChoosePlayDraw { .. }, GameAction::ChoosePlayDraw { .. })
        | (WaitingFor::TopOrBottomChoice { .. }, GameAction::ChooseTopOrBottom { .. })
        | (WaitingFor::ClashCardPlacement { .. }, GameAction::ChooseTopOrBottom { .. })
        | (WaitingFor::OptionalCostChoice { .. }, GameAction::DecideOptionalCost { .. })
        | (WaitingFor::DefilerPayment { .. }, GameAction::DecideOptionalCost { .. })
        | (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (WaitingFor::OpponentMayChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (WaitingFor::TributeChoice { .. }, GameAction::DecideOptionalEffect { .. })
        | (WaitingFor::UnlessPayment { .. }, GameAction::PayUnlessCost { .. })
        | (WaitingFor::CombatTaxPayment { .. }, GameAction::PayCombatTax { .. })
        | (WaitingFor::AdventureCastChoice { .. }, GameAction::ChooseAdventureFace { .. })
        | (WaitingFor::ModalFaceChoice { .. }, GameAction::ChooseModalFace { .. })
        | (WaitingFor::WarpCostChoice { .. }, GameAction::ChooseWarpCost { .. })
        | (WaitingFor::EvokeCostChoice { .. }, GameAction::ChooseEvokeCost { .. }) => false,
        (WaitingFor::MulliganBottomCards { player, count }, GameAction::SelectCards { cards }) => {
            selection_mismatch(
                cards,
                &state.players[player.0 as usize].hand,
                Some((*count).into()),
            )
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
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::ChooseFromZoneChoice {
                player: _,
                cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::DiscardForCost {
                player: _,
                cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::SacrificeForCost {
                player: _,
                permanents: cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::ReturnToHandForCost {
                player: _,
                permanents: cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::BlightChoice {
                player: _,
                creatures: cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        )
        | (
            WaitingFor::ExileFromGraveyardForCost {
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
        )
        | (
            WaitingFor::TapCreaturesForManaAbility {
                player: _,
                creatures: cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, Some(*count)),
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
            WaitingFor::ManifestDreadChoice { player: _, cards },
            GameAction::SelectCards { cards: chosen },
        ) => selection_mismatch(chosen, cards, Some(1)),
        (
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                ..
            },
            GameAction::DeclareAttackers { attacks },
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

/// Returns the legal actions for the current game state.
///
/// `TapLandForMana`/`UntapLandForMana` actions are filtered out — the frontend
/// derives land tappability from game state. Non-land mana abilities (dorks,
/// artifacts) are included so the frontend auto-pass system knows meaningful
/// actions exist. The AI uses `candidate_actions()` which excludes mana abilities
/// from priority candidates to keep the search tree clean.
/// Determines whether the frontend should auto-pass the current priority window.
///
/// Returns `true` when auto-passing is recommended:
/// - Only `PassPriority` is available (no spells, abilities, or lands to play)
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

    // Meaningful = any action that directly affects the game beyond passing.
    // Land mana abilities (ActivateAbility on a Land) are NOT meaningful on their
    // own — they only matter if the mana enables casting a spell, in which case
    // CastSpell will also be present in `actions` (the engine's can_pay_cost_after_auto_tap
    // already simulates tapping those lands when checking spell castability).
    let has_meaningful = actions.iter().any(|a| {
        match a {
            GameAction::PassPriority => false,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                // Non-mana activated abilities are always meaningful.
                // Mana abilities on lands are NOT meaningful on their own — they only
                // matter if the mana enables casting a spell, in which case CastSpell
                // will also be present in `actions`.
                state.objects.get(source_id).is_some_and(|obj| {
                    obj.abilities
                        .get(*ability_index)
                        .is_some_and(|ability| !mana_abilities::is_mana_ability(ability))
                })
            }
            _ => true,
        }
    });
    if !has_meaningful {
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

/// Returns legal actions, spell costs, AND a per-permanent action grouping.
///
/// `legal_actions_by_object` maps each permanent (or hand-zone card) to the
/// subset of legal actions whose `source_object()` equals that id. The grouping
/// is computed from the flat list via the engine-authoritative
/// `GameAction::source_object()` method — the frontend uses this map directly
/// instead of introspecting `GameAction` variants client-side.
pub fn legal_actions_full(state: &GameState) -> LegalActionsFull {
    let mut actions: Vec<GameAction> = validated_candidate_actions(state)
        .into_iter()
        .map(|candidate| candidate.action)
        .filter(|action| !action.is_mana_ability())
        .collect();

    // Build spell costs map from CastSpell actions.
    let mut spell_costs = HashMap::new();
    if let WaitingFor::Priority { player } = &state.waiting_for {
        for action in &actions {
            if let GameAction::CastSpell { object_id, .. } = action {
                if let Some(cost) =
                    crate::game::casting::effective_spell_cost(state, *player, *object_id)
                {
                    spell_costs.insert(*object_id, cost);
                }
            }
        }
    }

    // CR 605.3a: Append activatable mana abilities so the frontend knows the player
    // has meaningful actions beyond PassPriority. These are excluded from
    // candidate_actions() to keep the AI search tree clean (see candidates.rs
    // priority_actions), but the frontend needs them to avoid incorrect auto-pass.
    actions.extend(activatable_mana_ability_actions(state));

    // Group by source object using the engine-authoritative classifier.
    let mut grouped: HashMap<ObjectId, Vec<GameAction>> = HashMap::new();
    for action in &actions {
        if let Some(id) = action.source_object() {
            grouped.entry(id).or_default().push(action.clone());
        }
    }

    (actions, spell_costs, grouped)
}

/// CR 605.1b: Enumerate activatable mana abilities for the priority player.
///
/// Mirrors the per-ability scan pattern in `mana_sources::scan_mana_abilities` rather
/// than using the single `mana_ability_index` derived field, since a permanent may have
/// multiple mana abilities. Per-ability tap/sickness guards match `scan_mana_abilities`:
/// only abilities with a tap cost component require the permanent to be untapped and
/// free of summoning sickness (CR 302.6). Mana abilities don't use the stack (CR 605.3a).
fn activatable_mana_ability_actions(state: &GameState) -> Vec<GameAction> {
    let player = match &state.waiting_for {
        WaitingFor::Priority { player } => *player,
        _ => return Vec::new(),
    };

    let mut actions = Vec::new();
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.controller != player || !obj.has_mana_ability {
            continue;
        }
        for (idx, ability) in obj.abilities.iter().enumerate() {
            if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
                continue;
            }
            // CR 302.6: Only tap-cost abilities are gated by tapped state and summoning
            // sickness. Free or mana-cost-only mana abilities are always activatable.
            if mana_sources::has_tap_component(&ability.cost)
                && (obj.tapped || obj.has_summoning_sickness)
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
    use super::{
        candidate_actions, cheap_reject_candidate, legal_actions, legal_actions_full,
        validated_candidate_actions,
    };
    use crate::types::ability::ManaContribution;
    use crate::types::actions::GameAction;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn legal_actions_by_object_groups_flat_list_correctly() {
        // Flat list and grouped map are derived from the same actions; every
        // entry in the grouped map must equal source_object() of its actions,
        // and every action with Some(id) must appear under that id exactly once.
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
            obj.abilities.push(
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
                    },
                )
                .cost(AbilityCost::Tap),
            );
            // Non-mana ability (index 1)
            obj.abilities.push(AbilityDefinition::new(
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
    }
}
