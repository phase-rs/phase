use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::{combat, players};
use crate::types::ability::{
    DelayedTriggerCondition, DelayedTriggerKind, Effect, EffectError, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TokenAttackDestination,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 702.116a: Myriad creates a tapped attacking copy for each opponent
/// other than the defending player for the source creature. The current engine
/// supports Myriad's all-or-none "you may" choice and chooses the player
/// branch of "that player or a planeswalker they control"; per-opponent
/// declines and planeswalker redirection need a dedicated resolution choice UI.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, declarations| resolve_owned(state, executable, events, declarations),
    )
}

pub(crate) fn resolve_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::Myriad) {
        return Err(EffectError::MissingParam("Myriad".to_string()));
    }

    let Some(defending_player) = defending_player_for_myriad(state, ability) else {
        return super::token::no_token_owned(state, ability, events, declarations);
    };

    let opponents: Vec<PlayerId> = players::opponents(state, ability.controller)
        .into_iter()
        .filter(|opponent| *opponent != defending_player)
        .collect();
    let mut opponents: std::collections::VecDeque<_> = opponents.into();
    let (caller, work) = if let Some(current) = opponents.pop_front() {
        (
            crate::types::game_state::TokenCreationCallerState::Myriad {
                current,
                remaining: opponents,
                defender: defending_player,
            },
            copy_work(state, ability)?,
        )
    } else {
        (
            crate::types::game_state::TokenCreationCallerState::Plain,
            std::collections::VecDeque::new(),
        )
    };
    super::token_copy::begin_creation_owned(
        state,
        ability,
        work,
        crate::types::ability::EffectKind::Myriad,
        caller,
        events,
        declarations,
    )?;
    Ok(())
}

pub(crate) fn copy_work(
    state: &mut GameState,
    ability: &ResolvedAbility,
) -> Result<std::collections::VecDeque<crate::types::game_state::TokenCreationWork>, EffectError> {
    let copy = super::token_copy::own_source_copy_ability(
        Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            attack_destination: TokenAttackDestination::ControllerChooses,
            tapped: true,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
        ability,
    );
    super::token_copy::prepare_copy_creation(state, &copy)
}

pub(crate) fn complete_creation_segment(
    state: &mut GameState,
    opponent: PlayerId,
    members: &[crate::types::identifiers::ObjectIncarnationRef],
) {
    for member in members {
        let Some(object) = state.objects.get(&member.object_id) else {
            continue;
        };
        if crate::types::identifiers::ObjectIncarnationRef::from_object(object) != *member
            || object.zone != Zone::Battlefield
            || !object
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Creature)
            || object
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Battle)
        {
            continue;
        }
        let target = combat::AttackTarget::Player(opponent);
        if combat::valid_entry_attack_targets(
            state,
            object.controller,
            &crate::types::ability::EntryAttackDestination::AnyDefender,
        )
        .contains(&target)
        {
            combat::enter_attacking_at_target(state, member.object_id, opponent, target);
        }
    }
}

pub(crate) fn complete_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    result: &crate::types::ability::ForwardedResultContext,
    events: &mut Vec<GameEvent>,
) {
    if result.object_incarnations.is_empty() {
        return;
    }
    let mut payload = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target: TargetFilter::LastCreated,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        ability.source_id,
        ability.controller,
    );
    super::delayed_trigger::freeze_created_result(&mut payload, result);
    crate::game::triggers::install_delayed_trigger(
        state,
        DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            },
            ability: Box::new(payload),
            controller: ability.controller,
            source_id: ability.source_id,
            one_shot: true,
            kind: DelayedTriggerKind::Ordinary,
            provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
        },
        events,
    );
}

fn defending_player_for_myriad(state: &GameState, ability: &ResolvedAbility) -> Option<PlayerId> {
    ability
        .targets
        .iter()
        .find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            TargetRef::Object(_) => None,
        })
        .or_else(|| {
            defending_player_from_attack_event(
                state.current_trigger_event.as_ref(),
                ability.source_id,
            )
        })
        .or_else(|| combat::defending_player_for_attacker(state, ability.source_id))
}

pub(crate) fn defending_player_from_attack_event(
    event: Option<&GameEvent>,
    source_id: ObjectId,
) -> Option<PlayerId> {
    let GameEvent::AttackersDeclared {
        attacker_ids,
        defending_player,
        attacks,
        ..
    } = event?
    else {
        return None;
    };

    attacks
        .iter()
        .find_map(|(attacker_id, _)| (*attacker_id == source_id).then_some(*defending_player))
        .or_else(|| {
            attacker_ids
                .contains(&source_id)
                .then_some(*defending_player)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn myriad_completion_synchronous() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Myriad,
            super::super::token_copy::tests::CreationPauseCase::Synchronous,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        for id in &state.last_created_token_ids {
            let attacker = state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == *id)
                .unwrap();
            assert_eq!(attacker.defending_player, crate::types::player::PlayerId(2));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Myriad,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn myriad_completion_queued_child() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Myriad,
            super::super::token_copy::tests::CreationPauseCase::Queued,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        for id in &state.last_created_token_ids {
            let attacker = state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == *id)
                .unwrap();
            assert_eq!(attacker.defending_player, crate::types::player::PlayerId(2));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Myriad,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn myriad_completion_counter_pause() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Myriad,
            super::super::token_copy::tests::CreationPauseCase::Counter,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        for id in &state.last_created_token_ids {
            let attacker = state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == *id)
                .unwrap();
            assert_eq!(attacker.defending_player, crate::types::player::PlayerId(2));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Myriad,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn myriad_completion_queued_child_current_reload() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Myriad,
            super::super::token_copy::tests::CreationPauseCase::ReloadQueued,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        for id in &state.last_created_token_ids {
            let attacker = state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == *id)
                .unwrap();
            assert_eq!(attacker.defending_player, crate::types::player::PlayerId(2));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Myriad,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn myriad_completion_counter_current_reload() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Myriad,
            super::super::token_copy::tests::CreationPauseCase::ReloadCounter,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        for id in &state.last_created_token_ids {
            let attacker = state
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == *id)
                .unwrap();
            assert_eq!(attacker.defending_player, crate::types::player::PlayerId(2));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Myriad,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
}
