//! Unstable Contraption deck, assemble, and crank runtime.

use crate::game::effects::choose_one_of;
use crate::game::effects::gain_control;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::GameObject;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectError, EffectKind, ReassembleControlMode,
    ResolvedAbility, SubAbilityLink, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

pub fn is_contraption_card(obj: &GameObject) -> bool {
    obj.in_contraption_deck
        || obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype.eq_ignore_ascii_case("Contraption"))
}

pub fn is_contraption_permanent(obj: &GameObject) -> bool {
    obj.zone == Zone::Battlefield && is_contraption_card(obj)
}

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::AssembleContraptions { count } => {
            let count = resolve_quantity_with_targets(state, count, ability).max(0) as u32;
            start_assemble_batch(
                state,
                ability.controller,
                ability.source_id,
                count,
                true,
                events,
            );
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::AssembleContraptions,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        Effect::AssembleContraptionsFromRollDifference => {
            let count = recent_roll_difference(events);
            start_assemble_batch(
                state,
                ability.controller,
                ability.source_id,
                count,
                true,
                events,
            );
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::AssembleContraptionsFromRollDifference,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        Effect::AssembleContraptionOnSprocket {
            target,
            sprocket,
            remaining,
        } => {
            assemble_one_onto_sprocket(
                state,
                target,
                ability.source_id,
                *sprocket,
                *remaining,
                events,
            )?;
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::AssembleContraptionOnSprocket,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        Effect::CrankContraptions { target } => {
            crank_selected_contraptions(state, ability, target, events);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CrankContraptions,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        Effect::ReassembleContraption {
            target,
            control_mode,
        } => {
            prompt_reassemble_sprocket_choice(state, ability, target, *control_mode)?;
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ReassembleContraption,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        Effect::ReassembleContraptionOnSprocket {
            target,
            sprocket,
            control_mode,
        } => {
            apply_reassemble_to_sprocket(state, ability, target, *sprocket, *control_mode, events)?;
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ReassembleContraptionOnSprocket,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        _ => Err(EffectError::MissingParam("Contraptions".to_string())),
    }
}

pub fn perform_contraption_upkeep_turn_based_action(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let player = state.active_player;
    if !controls_contraption(state, player) {
        return None;
    }

    let sprocket = advance_crank_sprocket(state, player);
    let eligible: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == player
                    && obj.is_phased_in()
                    && is_contraption_permanent(obj)
                    && obj.contraption_sprocket == Some(sprocket)
            })
        })
        .map(crate::types::ability::TargetRef::Object)
        .collect();

    if eligible.is_empty() {
        return None;
    }

    let continuation = ResolvedAbility::new(
        Effect::CrankContraptions {
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
        },
        Vec::new(),
        ObjectId(0),
        player,
    );
    state.park_ability_continuation(PendingContinuation::new(Box::new(continuation), state));
    state.waiting_for = WaitingFor::ChooseObjectsSelection {
        player,
        eligible,
        trigger_event: None,
    };
    let _ = events;
    Some(state.waiting_for.clone())
}

pub(crate) fn finish_contraption_assembly(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    sprocket: u8,
    events: &mut Vec<GameEvent>,
) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.in_contraption_deck = false;
        obj.contraption_sprocket = (obj.zone == Zone::Battlefield).then_some(sprocket);
    }
    if state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield)
    {
        events.push(GameEvent::ContraptionAssembled {
            player_id: player,
            object_id,
            sprocket,
        });
    }
}

fn start_assemble_batch(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    count: u32,
    apply_replacements: bool,
    events: &mut Vec<GameEvent>,
) {
    let count = if apply_replacements {
        apply_assemble_replacements(state, source_id, count)
    } else {
        count
    };
    let available = available_contraptions(state, player);
    let count = count.min(available);
    if count == 0 {
        return;
    }

    continue_assemble_batch(state, player, source_id, count, events);
}

pub(crate) fn continue_assemble_batch(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    remaining: u32,
    events: &mut Vec<GameEvent>,
) {
    if remaining == 0 {
        return;
    }
    let Some(object_id) = top_contraption_id(state, player) else {
        return;
    };
    reveal_contraption(state, player, object_id, events);
    choose_one_of::prompt_next(
        state,
        choose_one_of::PromptRequest {
            controller: player,
            source_id,
            branches: assemble_sprocket_branches(object_id, remaining),
            parent_targets: Vec::new(),
            context: crate::types::ability::SpellContext::default(),
            replacement_applied: Default::default(),
            players: vec![player],
        },
    );
}

fn assemble_sprocket_branches(object_id: ObjectId, remaining: u32) -> Vec<AbilityDefinition> {
    [1_u8, 2, 3]
        .into_iter()
        .map(|sprocket| {
            let mut branch = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::AssembleContraptionOnSprocket {
                    target: TargetFilter::SpecificObject { id: object_id },
                    sprocket,
                    remaining: remaining.saturating_sub(1),
                },
            )
            .description(format!("Put it onto sprocket {sprocket}."));
            branch.sub_link = SubAbilityLink::SequentialSibling;
            branch
        })
        .collect()
}

fn assemble_one_onto_sprocket(
    state: &mut GameState,
    target: &TargetFilter,
    source_id: ObjectId,
    sprocket: u8,
    remaining: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let object_id = match target {
        TargetFilter::SpecificObject { id } => *id,
        _ => {
            return Err(EffectError::InvalidParam(
                "assemble target must be a specific Contraption".to_string(),
            ))
        }
    };
    let Some(player) = state.objects.get(&object_id).map(|obj| obj.owner) else {
        return Err(EffectError::ObjectNotFound(object_id));
    };
    let Some(front_id) = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .and_then(|candidate| candidate.contraption_deck.front().copied())
    else {
        return Ok(());
    };
    if front_id != object_id {
        return Err(EffectError::InvalidParam(format!(
            "assemble target {object_id:?} is not the top Contraption"
        )));
    }
    state
        .players
        .iter_mut()
        .find(|candidate| candidate.id == player)
        .and_then(|candidate| candidate.contraption_deck.pop_front())
        .expect("front Contraption was present");

    match super::zone_pipeline::move_object(
        state,
        super::zone_pipeline::ZoneMoveRequest::effect(object_id, Zone::Battlefield, source_id),
        events,
    ) {
        super::zone_pipeline::ZoneMoveResult::Done => {
            finish_contraption_assembly(state, player, object_id, sprocket, events);
            if remaining > 0 {
                continue_assemble_batch(state, player, source_id, remaining, events);
            }
        }
        super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
            super::zone_pipeline::defer_completion_on_pause(
                state,
                BatchCompletion::ContraptionAssembleRemainder {
                    player,
                    source_id,
                    object_id,
                    sprocket,
                    remaining_after: remaining,
                },
            );
        }
    }
    Ok(())
}

fn crank_selected_contraptions(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    events: &mut Vec<GameEvent>,
) {
    let mut contraptions = resolved_object_ids_for_filter(state, ability, target);
    contraptions.sort_by_key(|id| id.0);
    contraptions.dedup();

    for contraption_id in contraptions {
        let Some(obj) = state.objects.get(&contraption_id) else {
            continue;
        };
        if obj.zone != Zone::Battlefield || !obj.is_phased_in() || !is_contraption_permanent(obj) {
            continue;
        }
        let Some(sprocket) = obj.contraption_sprocket else {
            continue;
        };
        events.push(GameEvent::ContraptionCranked {
            player_id: obj.controller,
            sprocket,
            contraption_id,
        });
    }

    super::triggers::process_triggers(state, events);
}

fn prompt_reassemble_sprocket_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    control_mode: ReassembleControlMode,
) -> Result<(), EffectError> {
    let mut targets = resolved_object_ids_for_filter(state, ability, target);
    targets.sort_by_key(|id| id.0);
    targets.dedup();
    let Some(target_id) = targets.first().copied() else {
        return Ok(());
    };
    let current_sprocket = state
        .objects
        .get(&target_id)
        .and_then(|obj| obj.contraption_sprocket);
    let changing_controller = matches!(control_mode, ReassembleControlMode::GainControl)
        && state
            .objects
            .get(&target_id)
            .is_some_and(|obj| obj.controller != ability.controller);
    let branches: Vec<_> = [1_u8, 2, 3]
        .into_iter()
        .filter(|sprocket| changing_controller || Some(*sprocket) != current_sprocket)
        .map(|sprocket| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ReassembleContraptionOnSprocket {
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    sprocket,
                    control_mode,
                },
            )
            .description(format!("Move it onto sprocket {sprocket}."))
        })
        .collect();
    if branches.is_empty() {
        return Ok(());
    }

    state
        .tracked_object_sets
        .insert(TrackedSetId(0), vec![target_id]);
    state.chain_tracked_set_id = Some(TrackedSetId(0));
    choose_one_of::prompt_next(
        state,
        choose_one_of::PromptRequest {
            controller: ability.controller,
            source_id: ability.source_id,
            branches,
            parent_targets: ability.targets.clone(),
            context: ability.context.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            players: vec![ability.controller],
        },
    );
    Ok(())
}

fn apply_reassemble_to_sprocket(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    sprocket: u8,
    control_mode: ReassembleControlMode,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut targets = resolved_object_ids_for_filter(state, ability, target);
    targets.sort_by_key(|id| id.0);
    targets.dedup();
    let Some(target_id) = targets.first().copied() else {
        return Ok(());
    };
    let controller_changed = {
        let Some(obj) = state.objects.get_mut(&target_id) else {
            return Ok(());
        };
        if obj.zone != Zone::Battlefield || !is_contraption_card(obj) {
            return Ok(());
        }
        matches!(control_mode, ReassembleControlMode::GainControl)
            && obj.controller != ability.controller
    };
    if matches!(control_mode, ReassembleControlMode::GainControl) {
        gain_control::apply_permanent_control_change(
            state,
            ability.source_id,
            target_id,
            ability.controller,
            events,
        );
        crate::game::layers::evaluate_layers(state);
    }
    if let Some(obj) = state.objects.get_mut(&target_id) {
        if !controller_changed || obj.controller == ability.controller {
            obj.contraption_sprocket = Some(sprocket);
        }
    }
    Ok(())
}

fn controls_contraption(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            obj.controller == player && obj.is_phased_in() && is_contraption_permanent(obj)
        })
    })
}

fn advance_crank_sprocket(state: &mut GameState, player: PlayerId) -> u8 {
    let player_state = state
        .players
        .iter_mut()
        .find(|candidate| candidate.id == player)
        .expect("active player exists");
    let next = next_sprocket(player_state.contraption_crank_sprocket);
    player_state.contraption_crank_sprocket = next;
    next
}

fn next_sprocket(current: u8) -> u8 {
    match current {
        1 => 2,
        2 => 3,
        _ => 1,
    }
}

fn recent_roll_difference(events: &[GameEvent]) -> u32 {
    let mut rolls = events.iter().rev().filter_map(|event| match event {
        GameEvent::DieRolled {
            result: Some(result),
            ..
        } => Some(*result),
        _ => None,
    });
    let Some(first) = rolls.next() else {
        return 0;
    };
    let Some(second) = rolls.next() else {
        return 0;
    };
    u8::abs_diff(first, second) as u32
}

fn available_contraptions(state: &GameState, player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .map(|candidate| candidate.contraption_deck.len() as u32)
        .unwrap_or(0)
}

fn top_contraption_id(state: &GameState, player: PlayerId) -> Option<ObjectId> {
    state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .and_then(|candidate| candidate.contraption_deck.front().copied())
}

fn reveal_contraption(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let Some(card_name) = state.objects.get(&object_id).map(|obj| obj.name.clone()) else {
        return;
    };
    state.last_revealed_ids = vec![object_id];
    events.push(GameEvent::CardsRevealed {
        player,
        card_ids: vec![object_id],
        card_names: vec![card_name],
    });
}

fn apply_assemble_replacements(state: &GameState, source_id: ObjectId, count: u32) -> u32 {
    use crate::types::ability::QuantityModification;

    let mut adjusted = count;
    for (replacement_source_id, replacement_source) in &state.objects {
        if replacement_source.zone != Zone::Battlefield || !replacement_source.is_phased_in() {
            continue;
        }
        for replacement in replacement_source.replacement_definitions.iter_unchecked() {
            if replacement.event
                != crate::types::replacements::ReplacementEvent::AssembleContraption
            {
                continue;
            }
            let matches_source = replacement.valid_card.as_ref().is_none_or(|filter| {
                let ctx = FilterContext::from_source_with_controller(
                    *replacement_source_id,
                    replacement_source.controller,
                );
                matches_target_filter(state, source_id, filter, &ctx)
            });
            if matches_source {
                adjusted = match replacement.quantity_modification {
                    Some(QuantityModification::Times { factor }) => adjusted.saturating_mul(factor),
                    Some(QuantityModification::Half) => adjusted / 2,
                    Some(QuantityModification::Plus { value }) => adjusted.saturating_add(value),
                    Some(QuantityModification::Minus { value }) => adjusted.saturating_sub(value),
                    Some(QuantityModification::Prevent) => 0,
                    None => adjusted,
                };
            }
        }
    }
    adjusted
}
