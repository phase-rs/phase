use crate::types::ability::{
    CardPlayMode, Duration, Effect, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

/// CR 702.88a: Rebound — on-resolution arming hook for an instant/sorcery
/// spell cast from its owner's hand. Called from `stack.rs::resolve_top` after
/// it has confirmed the spell carries `Keyword::Rebound`, was cast from hand,
/// is not a token, and is not a permanent spell.
// CR 603.7a: Creates the rebound delayed triggered ability when the spell
// resolves.
// CR 603.7d: Source of the delayed trigger is the exiled spell card; the
// trigger's controller is the player who controlled the resolving spell.
// CR 603.7b: The delayed trigger has no stated duration — it fires once at
// the controller's next upkeep and is then removed (`one_shot = true`).
///
/// Action: push a `DelayedTrigger` keyed on
/// `AtNextPhaseForPlayer { Phase::Upkeep, controller }` whose body is an
/// optional `Effect::CastFromZone` that targets the exiled card itself
/// (`TargetRef::Object(exiled_id)`), uses `without_paying_mana_cost: true`,
/// and carries `Duration::UntilEndOfTurn` so the granted recast permission
/// is pruned at end of turn if the controller declines or fails to cast
/// (CR 514.2 + CR 611.2a).
///
/// Returns `true` so the caller can override the spell's post-resolution
/// destination from graveyard to exile (CR 608.2n displaced by the Rebound
/// reminder text). Never fails — gating is performed by the caller.
pub fn arm_rebound(state: &mut GameState, exiled_id: ObjectId, controller: PlayerId) -> bool {
    // CR 702.88a: at the beginning of your next upkeep, you may cast this
    // card from exile without paying its mana cost.
    let mut inner = ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::SelfRef,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            // CR 514.2: the granted "cast from exile without paying" permission
            // expires at end of turn if the controller declines or fails to
            // cast, so a leftover Rebound permission cannot leak into a later
            // turn.
            duration: Some(Duration::UntilEndOfTurn),
            driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
        },
        vec![TargetRef::Object(exiled_id)],
        exiled_id,
        controller,
    );
    // CR 702.88a: "you may cast" is an optional effect.
    inner.optional = true;

    state.delayed_triggers.push(DelayedTrigger {
        // CR 603.7b: fires once at the controller's next upkeep.
        condition: crate::types::ability::DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::Upkeep,
            player: controller,
        },
        ability: inner,
        // CR 603.7d: controller of the delayed trigger is the player who
        // controlled the resolving Rebound spell.
        controller,
        // CR 603.7d: the source of the delayed trigger is the spell that
        // created it; using `exiled_id` ties the trigger to the now-exiled
        // card object.
        source_id: exiled_id,
        // CR 603.7b: one-shot — removed after it fires.
        one_shot: true,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{CastingPermission, DelayedTriggerCondition};
    use crate::types::game_state::GameState;

    #[test]
    fn arm_rebound_pushes_delayed_trigger_with_optional_cast() {
        let mut state = GameState::new_two_player(42);
        let exiled = ObjectId(100);
        let controller = PlayerId(0);
        assert!(arm_rebound(&mut state, exiled, controller));
        assert_eq!(state.delayed_triggers.len(), 1);
        let trig = &state.delayed_triggers[0];
        // CR 603.7b: keyed on the controller's next upkeep.
        match &trig.condition {
            DelayedTriggerCondition::AtNextPhaseForPlayer { phase, player } => {
                assert_eq!(*phase, Phase::Upkeep);
                assert_eq!(*player, controller);
            }
            other => panic!("expected AtNextPhaseForPlayer Upkeep, got {other:?}"),
        }
        // CR 603.7b: one-shot.
        assert!(trig.one_shot);
        // CR 603.7d: controller and source match the arming call.
        assert_eq!(trig.controller, controller);
        assert_eq!(trig.source_id, exiled);
        // CR 702.88a: the body offers "you may" — optional.
        assert!(trig.ability.optional);
        // CR 702.88a: the body targets the exiled card via TargetRef::Object.
        assert_eq!(trig.ability.targets, vec![TargetRef::Object(exiled)]);
    }

    #[test]
    fn two_rebound_arms_push_independent_triggers() {
        let mut state = GameState::new_two_player(42);
        assert!(arm_rebound(&mut state, ObjectId(100), PlayerId(0)));
        assert!(arm_rebound(&mut state, ObjectId(101), PlayerId(0)));
        // CR 603.7a: each resolution creates a separate delayed trigger.
        assert_eq!(state.delayed_triggers.len(), 2);
        assert_eq!(state.delayed_triggers[0].source_id, ObjectId(100));
        assert_eq!(state.delayed_triggers[1].source_id, ObjectId(101));
    }

    #[test]
    fn armed_cast_effect_carries_until_end_of_turn_duration() {
        let mut state = GameState::new_two_player(42);
        let exiled = ObjectId(200);
        let controller = PlayerId(1);
        arm_rebound(&mut state, exiled, controller);
        let trig = &state.delayed_triggers[0];
        // CR 514.2: the granted permission must carry UntilEndOfTurn so it
        // is pruned at cleanup if the controller declines the optional cast.
        match &trig.ability.effect {
            Effect::CastFromZone {
                without_paying_mana_cost,
                duration,
                target,
                ..
            } => {
                assert!(*without_paying_mana_cost);
                assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
                assert_eq!(*target, TargetFilter::SelfRef);
            }
            other => panic!("expected CastFromZone body, got {other:?}"),
        }
    }

    // CR 702.88a propagation: the durational permission is plumbed through
    // `cast_from_zone::resolve` so that the granted `ExileWithAltCost`
    // permission inherits `duration: Some(UntilEndOfTurn)`. Exercise the
    // plumbing here to lock the contract that armed triggers grant a
    // pruneable permission rather than a standing one.
    #[test]
    fn cast_from_zone_propagates_rebound_duration_to_granted_permission() {
        use crate::game::effects::cast_from_zone;
        use crate::game::zones::create_object;
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let owner = PlayerId(0);
        let exiled = create_object(
            &mut state,
            CardId(123),
            owner,
            "Rebound Card".to_string(),
            Zone::Exile,
        );

        // CR 702.88a: simulate the arming flow's body — the same effect that
        // `arm_rebound` constructs and resolves via the delayed trigger.
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::SelfRef,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: Some(Duration::UntilEndOfTurn),
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            vec![TargetRef::Object(exiled)],
            exiled,
            owner,
        );

        let mut events: Vec<GameEvent> = Vec::new();
        cast_from_zone::resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&exiled).unwrap();
        let armed_perm = obj
            .casting_permissions
            .iter()
            .find_map(|p| match p {
                CastingPermission::ExileWithAltCost {
                    duration: Some(d), ..
                } => Some(d.clone()),
                _ => None,
            })
            .expect("CastFromZone must propagate duration onto the granted permission");
        // CR 514.2: the permission inherits the Rebound recast's UntilEndOfTurn
        // so the layer prune helpers expire it at the same turn's cleanup.
        assert_eq!(armed_perm, Duration::UntilEndOfTurn);
    }
}
