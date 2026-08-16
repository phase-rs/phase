use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 613.4d: Switch a creature's power and toughness. Registers a transient
/// continuous effect in layer 7d so the swap survives layer recalculation and
/// expires at the correct time.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::SwitchPT { target } => target,
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);
    let target_filter = super::resolved_object_filter(ability, target_filter);

    // CR 608.2c + 603.10a: Delegate to the unified 3-tier dispatch so `SelfRef`
    // resolves to the source object regardless of `ability.targets` (issue #323
    // class — chained `SwitchPT { target: SelfRef }` sub-abilities would
    // otherwise inherit the parent's targets via chain propagation).
    let effective_targets =
        crate::game::targeting::resolved_targets(ability, &target_filter, state);
    let ids = super::effect_object_targets(&target_filter, &effective_targets);

    for obj_id in ids {
        // CR 608.2b: If a target has left the battlefield, skip it.
        if !state.battlefield.contains(&obj_id) {
            continue;
        }
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            dur.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::SwitchPowerToughness],
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetRef;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_creature(
        state: &mut GameState,
        name: &str,
        power: i32,
        toughness: i32,
        owner: PlayerId,
    ) -> ObjectId {
        let id = create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    /// CR 613.4d (issue #323 class): a chained `SwitchPT { target: SelfRef }`
    /// sub-ability must switch the source object's power/toughness even when
    /// chain target propagation in `effects::mod.rs::resolve_ability_chain`
    /// injected the parent's targets into `ability.targets`. Pre-fix the
    /// resolver checked `SelfRef && ability.targets.is_empty()` locally; a
    /// propagated parent target would route through the `ability.targets`
    /// branch and switch the wrong creature.
    #[test]
    fn switch_pt_selfref_overrides_propagated_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let source = make_creature(&mut state, "Source", 1, 4, PlayerId(0));
        let other = make_creature(&mut state, "Other", 5, 2, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::SwitchPT {
                target: TargetFilter::SelfRef,
            },
            // Simulate chain target propagation from a parent that targeted
            // `other`. SelfRef must override and switch the source instead.
            vec![TargetRef::Object(other)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // Source switched 1/4 → 4/1.
        assert_eq!(state.objects[&source].power, Some(4));
        assert_eq!(state.objects[&source].toughness, Some(1));
        // Other unchanged.
        assert_eq!(state.objects[&other].power, Some(5));
        assert_eq!(state.objects[&other].toughness, Some(2));
    }
}
