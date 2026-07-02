use crate::game::{combat, players};
use crate::types::ability::{
    DelayedTriggerCondition, Effect, EffectError, QuantityExpr, ResolvedAbility, TargetFilter,
    TargetRef,
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
    if !matches!(ability.effect, Effect::Myriad) {
        return Err(EffectError::MissingParam("Myriad".to_string()));
    }

    let Some(defending_player) = defending_player_for_myriad(state, ability) else {
        events.push(GameEvent::EffectResolved {
            kind: crate::types::ability::EffectKind::Myriad,
            source_id: ability.source_id,
        });
        return Ok(());
    };

    let opponents: Vec<PlayerId> = players::opponents(state, ability.controller)
        .into_iter()
        .filter(|opponent| *opponent != defending_player)
        .collect();
    let mut created = Vec::new();

    for opponent in opponents {
        // CR 702.116a: Myriad copies are created under the source creature's
        // controller's control, so `owner` stays the default `Controller`.
        let copy_effect = Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: true,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        };
        let copy_ability =
            ResolvedAbility::new(copy_effect, vec![], ability.source_id, ability.controller);
        crate::game::effects::token_copy::resolve(state, &copy_ability, events)?;

        let token_ids = state.last_created_token_ids.clone();
        for token_id in token_ids {
            combat::place_attacking_alongside(
                state,
                token_id,
                opponent,
                combat::AttackTarget::Player(opponent),
                events,
            );
            created.push(token_id);
        }
    }

    if !created.is_empty() {
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            },
            ability: ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::Any,
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
                created
                    .iter()
                    .copied()
                    .map(crate::types::ability::TargetRef::Object)
                    .collect(),
                ability.source_id,
                ability.controller,
            ),
            controller: ability.controller,
            source_id: ability.source_id,
            one_shot: true,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::Myriad,
        source_id: ability.source_id,
    });
    Ok(())
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
