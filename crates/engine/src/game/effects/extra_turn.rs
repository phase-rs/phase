use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

// IMPLEMENTATION BUDGET BOUND: resolving one effect must not allocate an
// attacker-controlled number of queued turns and events. This is an engine
// resource ceiling, not a restriction imposed by the Comprehensive Rules.
pub(crate) const MAX_EXTRA_TURNS_PER_RESOLUTION: i32 = 1_000;

/// CR 500.7: Grant an extra turn to the resolved target player.
/// Extra turns are stored as a LIFO stack — push to end, pop from end.
/// The most recently created extra turn is taken first.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExtraTurn { target, count } = &ability.effect else {
        return Err(EffectError::MissingParam(
            "expected ExtraTurn effect".into(),
        ));
    };

    // CR 500.7: Resolve the target to a PlayerId.
    let player = match target {
        TargetFilter::Controller | TargetFilter::SelfRef => ability.controller,
        _ => {
            // Targeted variant: resolve from ability.targets
            if let Some(TargetRef::Player(pid)) = ability.targets.first() {
                *pid
            } else {
                // Fallback to controller if no target resolved
                ability.controller
            }
        }
    };

    // CR 107.1b: a negative calculated effect result is treated as zero.
    let count = resolve_quantity_with_targets(state, count, ability).max(0);
    if count > MAX_EXTRA_TURNS_PER_RESOLUTION {
        tracing::warn!(
            source_id = ?ability.source_id,
            count,
            limit = MAX_EXTRA_TURNS_PER_RESOLUTION,
            "rejecting oversized extra-turn resolution"
        );
        return Err(EffectError::InvalidParam(format!(
            "extra turn count {count} exceeds the per-resolution limit of {MAX_EXTRA_TURNS_PER_RESOLUTION}"
        )));
    }
    // CR 500.7: add multiple extra turns one at a time after the same specified turn.
    let anchor = state.active_player;
    for _ in 0..count {
        crate::game::turns::enqueue_extra_turn(state, player, anchor, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExtraTurn,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, TargetRef};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::ExtraTurn;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn et(player: u8, anchor: u8) -> ExtraTurn {
        ExtraTurn {
            player: PlayerId(player),
            anchor: PlayerId(anchor),
        }
    }

    fn make_ability_with_count(
        target: TargetFilter,
        count: QuantityExpr,
        controller: PlayerId,
    ) -> ResolvedAbility {
        ResolvedAbility {
            detached_remainder: crate::types::ability::DetachedRemainder::NoProducer,
            effect: Effect::ExtraTurn { target, count },
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            cast_occurrence: None,
            source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
            selected_target_incarnations: Vec::new(),
            illegal_target_slots: Vec::new(),
            targets: vec![],
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_player: None,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            modal_instruction_ordinal: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            distribute: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            sibling_condition: crate::types::ability::SiblingCondition::Dependent,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
        }
    }

    fn make_ability(target: TargetFilter, controller: PlayerId) -> ResolvedAbility {
        make_ability_with_count(target, QuantityExpr::Fixed { value: 1 }, controller)
    }

    #[test]
    fn extra_turn_pushes_controller_to_stack() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(0, 0)]);
        assert_eq!(
            events,
            vec![
                GameEvent::ExtraTurnCreated {
                    player_id: PlayerId(0),
                    anchor: PlayerId(0),
                },
                GameEvent::EffectResolved {
                    kind: EffectKind::ExtraTurn,
                    source_id: ObjectId(1),
                    subject: None,
                },
            ]
        );
        let GameEvent::ExtraTurnCreated { player_id, anchor } = &events[0] else {
            unreachable!();
        };
        assert_eq!(
            (*player_id, *anchor),
            (state.extra_turns[0].player, state.extra_turns[0].anchor)
        );
    }

    #[test]
    fn extra_turn_lifo_ordering() {
        let mut state = GameState::default();
        let mut events = Vec::new();

        // Player A takes an extra turn
        let ability_a = make_ability(TargetFilter::Controller, PlayerId(0));
        resolve(&mut state, &ability_a, &mut events).unwrap();

        // Player B takes an extra turn (most recent)
        let ability_b = make_ability(TargetFilter::Controller, PlayerId(1));
        resolve(&mut state, &ability_b, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(0, 0), et(1, 0)]);

        // CR 500.7: Pop from end → most recent (Player B) first
        assert_eq!(state.extra_turns.pop().map(|e| e.player), Some(PlayerId(1)));
        assert_eq!(state.extra_turns.pop().map(|e| e.player), Some(PlayerId(0)));
    }

    #[test]
    fn extra_turn_targeted_player() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let mut ability = make_ability(TargetFilter::Any, PlayerId(0));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(1, 0)]);
    }

    #[test]
    fn extra_turn_stores_active_player_as_anchor() {
        let mut state = GameState {
            active_player: PlayerId(2),
            ..Default::default()
        };
        let mut events = Vec::new();
        let ability = make_ability(TargetFilter::Controller, PlayerId(0));

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.extra_turns,
            vec![et(0, 2)],
            "CR 500.7: anchor is the specified (active) turn, not the beneficiary"
        );
    }

    #[test]
    fn two_hg_extra_turn_normalizes_to_team_representative() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        let mut events = Vec::new();
        let mut ability = make_ability_with_count(
            TargetFilter::Any,
            QuantityExpr::Fixed { value: 2 },
            PlayerId(2),
        );
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(0, 0), et(0, 0)]);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::ExtraTurnCreated { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn counted_extra_turns_enqueue_each_unit_and_complete_once() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 0);
        state.active_player = PlayerId(2);
        state.extra_turns.push(et(0, 1));
        let mut events = Vec::new();
        let mut ability = make_ability_with_count(
            TargetFilter::Player,
            QuantityExpr::Fixed { value: 2 },
            PlayerId(0),
        );
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(0, 1), et(1, 2), et(1, 2)]);
        assert_eq!(
            events,
            vec![
                GameEvent::ExtraTurnCreated {
                    player_id: PlayerId(1),
                    anchor: PlayerId(2),
                },
                GameEvent::ExtraTurnCreated {
                    player_id: PlayerId(1),
                    anchor: PlayerId(2),
                },
                GameEvent::EffectResolved {
                    kind: EffectKind::ExtraTurn,
                    source_id: ObjectId(1),
                    subject: None,
                },
            ]
        );
        assert_eq!(
            state.extra_turns.pop().map(|turn| turn.player),
            Some(PlayerId(1))
        );
        assert_eq!(
            state.extra_turns.pop().map(|turn| turn.player),
            Some(PlayerId(1))
        );
        assert_eq!(
            state.extra_turns.pop().map(|turn| turn.player),
            Some(PlayerId(0))
        );
    }

    #[test]
    fn zero_extra_turns_still_complete_the_effect_once() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            QuantityExpr::Fixed { value: 0 },
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.extra_turns.is_empty());
        assert_eq!(
            events,
            vec![GameEvent::EffectResolved {
                kind: EffectKind::ExtraTurn,
                source_id: ObjectId(1),
                subject: None,
            }]
        );
    }

    #[test]
    fn dynamic_extra_turn_count_uses_the_resolving_ability_context() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let mut ability = make_ability_with_count(
            TargetFilter::Controller,
            QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::Variable { name: "X".into() },
            },
            PlayerId(0),
        );
        ability.chosen_x = Some(2);

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(0, 0), et(0, 0)]);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::ExtraTurnCreated { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn extra_turn_resolution_accepts_the_resource_limit() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability_with_count(
            TargetFilter::Controller,
            QuantityExpr::Fixed {
                value: MAX_EXTRA_TURNS_PER_RESOLUTION,
            },
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.extra_turns.len(),
            MAX_EXTRA_TURNS_PER_RESOLUTION as usize
        );
        assert_eq!(events.len(), MAX_EXTRA_TURNS_PER_RESOLUTION as usize + 1);
        assert!(matches!(
            events.last(),
            Some(GameEvent::EffectResolved { .. })
        ));
    }

    #[test]
    fn extra_turn_resolution_rejects_oversized_fixed_and_dynamic_counts_atomically() {
        let mut state = GameState::default();
        state.extra_turns.push(et(1, 0));
        let original_turns = state.extra_turns.clone();
        let mut events = vec![GameEvent::EffectResolved {
            kind: EffectKind::Draw,
            source_id: ObjectId(2),
            subject: None,
        }];
        let original_events = events.clone();
        let fixed = make_ability_with_count(
            TargetFilter::Controller,
            QuantityExpr::Fixed {
                value: MAX_EXTRA_TURNS_PER_RESOLUTION + 1,
            },
            PlayerId(0),
        );

        let error = resolve(&mut state, &fixed, &mut events).unwrap_err();

        assert!(matches!(error, EffectError::InvalidParam(_)));
        assert_eq!(state.extra_turns, original_turns);
        assert_eq!(events, original_events);

        let mut dynamic = make_ability_with_count(
            TargetFilter::Controller,
            QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::Variable { name: "X".into() },
            },
            PlayerId(0),
        );
        dynamic.chosen_x = Some(u32::MAX);

        let error = resolve(&mut state, &dynamic, &mut events).unwrap_err();

        assert!(matches!(error, EffectError::InvalidParam(_)));
        assert_eq!(state.extra_turns, original_turns);
        assert_eq!(events, original_events);
    }

    #[test]
    fn standard_extra_turn_targeted_player_is_not_normalized() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 0);
        let mut events = Vec::new();
        let mut ability = make_ability(TargetFilter::Any, PlayerId(0));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.extra_turns, vec![et(1, 0)]);
    }
}
