use crate::types::ability::{
    ContinuousModification, Duration, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 613.3: GainControl creates a transient continuous effect that changes the
/// target permanent's controller through the layer system (Layer 2).
///
/// The duration comes from the resolved ability: "until end of turn" → UntilEndOfTurn,
/// permanent control change → Permanent (indefinite). The layer system handles
/// reverting control when the effect expires.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 613.1b: Layer 2 — control-changing effects are applied.
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // Verify target exists
            if !state.objects.contains_key(obj_id) {
                return Err(EffectError::ObjectNotFound(*obj_id));
            }

            // CR 613.3: Create a transient continuous effect at Layer 2 (Control).
            // The affected filter targets this specific object by ID.
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificObject { id: *obj_id },
                vec![ContinuousModification::ChangeController],
                None,
            );
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 110.2: Give control of target permanent to a specified recipient player.
/// Unlike `resolve` (controller takes), this transfers to a different player
/// specified by the recipient target.
pub fn resolve_give(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    // The recipient is the player target; the object is the object target.
    let recipient_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Player(pid) = t {
                Some(*pid)
            } else {
                None
            }
        })
        .unwrap_or(ability.controller);

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            if !state.objects.contains_key(obj_id) {
                return Err(EffectError::ObjectNotFound(*obj_id));
            }

            // CR 613.3: Create a transient continuous effect at Layer 2 (Control)
            // with the recipient as the new controller.
            state.add_transient_continuous_effect(
                ability.source_id,
                recipient_id,
                duration.clone(),
                TargetFilter::SpecificObject { id: *obj_id },
                vec![ContinuousModification::ChangeController],
                None,
            );
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GiveControl,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_gain_control_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn gain_control_creates_transient_effect() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_gain_control_ability(target_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Verify a transient continuous effect was created
        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.controller, PlayerId(0));
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: target_id });
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::ChangeController]
        );
        assert!(state.layers_dirty);
    }

    /// CR 613.1b: Non-regression for Bug B (layer fix). After switching the
    /// ChangeController layer arm to trust `effect.controller` instead of
    /// `source.controller`, the standard gain-control flow (where caster is
    /// also source.controller) must still transfer control correctly through
    /// the full layer pipeline.
    #[test]
    fn gain_control_layer_pipeline_transfers_control() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        // Source (the Control Magic aura) is controlled by PlayerId(0) (the caster),
        // matching the real gain-control shape where source.controller == new controller.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Control Magic".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(0),
            "target should now be controlled by the caster after gain_control"
        );
    }

    #[test]
    fn gain_control_nonexistent_target_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_gain_control_ability(ObjectId(999));
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_err());
    }
}
