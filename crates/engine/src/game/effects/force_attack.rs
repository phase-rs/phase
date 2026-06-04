use super::{effect_object_targets, resolve_player_for_context_ref};
use crate::types::ability::{
    ContinuousModification, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::statics::StaticMode;

/// CR 508.1d: Force attack — the target creature must attack the required player
/// this turn/combat if able.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ForceAttack {
        target,
        required_player,
        duration,
    } = &ability.effect
    else {
        return Ok(());
    };

    let player = resolve_player_for_context_ref(state, ability, required_player);
    for obj_id in effect_object_targets(target, &ability.targets) {
        if !state.objects.contains_key(&obj_id) {
            continue;
        }

        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::MustAttackPlayer { player },
            }],
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ForceAttack,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Duration, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_force_attack_ability(
        source: ObjectId,
        target: ObjectId,
        controller: PlayerId,
        duration: Duration,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::Any,
                required_player: TargetFilter::Controller,
                duration,
            },
            vec![TargetRef::Object(target)],
            source,
            controller,
        )
    }

    #[test]
    fn force_attack_grants_must_attack_player_for_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Siren".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability =
            make_force_attack_ability(source, target, PlayerId(0), Duration::UntilEndOfCombat);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let effect = state
            .transient_continuous_effects
            .iter()
            .find(|ce| ce.affected == TargetFilter::SpecificObject { id: target })
            .expect("force attack should create a transient effect for the target");

        assert_eq!(effect.duration, Duration::UntilEndOfCombat);
        assert!(effect.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackPlayer { player },
                } if *player == PlayerId(0)
            )
        }));

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ForceAttack,
                source_id,
            } if *source_id == source
        )));
    }
}
