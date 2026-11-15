use crate::types::ability::{CardPlayMode, Effect, ResolvedAbility, TargetFilter, TargetRef};
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
/// (`TargetRef::Object(exiled_id)`) and casts it without paying its mana
/// cost AS the trigger resolves (`CastFromZoneDriver::DuringResolution`,
/// CR 608.2g) — the same shape as Suspend's last-counter cast
/// (CR 702.62a). Accept puts the spell on the stack immediately (with the
/// CR 608.2g timing bypass, so rebounding sorceries are castable at
/// upkeep); decline leaves the card in exile with no lingering permission.
///
/// Returns `true` so the caller can override the spell's post-resolution
/// destination from graveyard to exile (CR 608.2n displaced by the Rebound
/// reminder text). Never fails — gating is performed by the caller.
pub fn arm_rebound(
    state: &mut GameState,
    exiled_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<crate::types::events::GameEvent>,
) -> bool {
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
            // CR 608.2g: the recast offer names no duration, so the "you may
            // cast this card from exile" instruction executes AS the trigger
            // resolves — no lingering permission is ever granted (`duration:
            // None`), and declining leaves the card in exile. Mirrors
            // Suspend's last-counter cast (CR 702.62a, issue #1520); the
            // lingering `UntilEndOfTurn` permission this carried before let
            // the recast happen at any later priority window (issue #6461).
            duration: None,
            driver: crate::types::ability::CastFromZoneDriver::DuringResolution,
            mana_spend_permission: None,
            additional_cost: None,
            cast_cost_modifier: None,
        },
        vec![TargetRef::Object(exiled_id)],
        exiled_id,
        controller,
    );
    // CR 702.88a: "you may cast" is an optional effect.
    inner.optional = true;

    let rebound_cast = DelayedTrigger {
        // CR 603.7b: fires once at the controller's next upkeep.
        condition: crate::types::ability::DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::Upkeep,
            player: controller,
            gate: crate::types::ability::TurnGate::None,
            // Already-concrete `player` (constructed directly, never passed
            // through the placeholder-resolving `resolve()` path), so
            // `binding` is unread here; `Controller` is the accurate label.
            binding: crate::types::ability::DelayedTriggerPlayerBinding::Controller,
        },
        ability: Box::new(inner),
        // CR 603.7d: controller of the delayed trigger is the player who
        // controlled the resolving Rebound spell.
        controller,
        // CR 603.7d: the source of the delayed trigger is the spell that
        // created it; using `exiled_id` ties the trigger to the now-exiled
        // card object.
        source_id: exiled_id,
        // CR 603.7b: one-shot — removed after it fires.
        one_shot: true,
        kind: crate::types::ability::DelayedTriggerKind::Ordinary,
        provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
    };
    crate::game::triggers::install_delayed_trigger(state, rebound_cast, events);
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
        let mut events = Vec::new();
        assert!(arm_rebound(&mut state, exiled, controller, &mut events));
        assert_eq!(state.delayed_triggers.len(), 1);
        let trig = &state.delayed_triggers[0];
        // CR 603.7b: keyed on the controller's next upkeep.
        match &trig.condition {
            DelayedTriggerCondition::AtNextPhaseForPlayer { phase, player, .. } => {
                assert_eq!(phase, &Phase::Upkeep);
                assert_eq!(player, &controller);
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
        let mut events = Vec::new();
        assert!(arm_rebound(
            &mut state,
            ObjectId(100),
            PlayerId(0),
            &mut events
        ));
        assert!(arm_rebound(
            &mut state,
            ObjectId(101),
            PlayerId(0),
            &mut events
        ));
        // CR 603.7a: each resolution creates a separate delayed trigger.
        assert_eq!(state.delayed_triggers.len(), 2);
        assert_eq!(state.delayed_triggers[0].source_id, ObjectId(100));
        assert_eq!(state.delayed_triggers[1].source_id, ObjectId(101));
    }

    #[test]
    fn armed_cast_effect_casts_during_resolution_without_lingering_duration() {
        let mut state = GameState::new_two_player(42);
        let exiled = ObjectId(200);
        let controller = PlayerId(1);
        let mut events = Vec::new();
        arm_rebound(&mut state, exiled, controller, &mut events);
        let trig = &state.delayed_triggers[0];
        // CR 702.88a + CR 608.2g: the offer names no duration, so the body
        // casts as the trigger resolves (`DuringResolution`) with no
        // lingering permission to expire (`duration: None`).
        match &trig.ability.effect {
            Effect::CastFromZone {
                without_paying_mana_cost,
                duration,
                target,
                driver,
                ..
            } => {
                assert!(*without_paying_mana_cost);
                assert_eq!(*duration, None);
                assert_eq!(*target, TargetFilter::SelfRef);
                assert!(driver.is_during_resolution());
            }
            other => panic!("expected CastFromZone body, got {other:?}"),
        }
    }

    // CR 702.88a + CR 608.2g: resolving the armed trigger body casts the
    // exiled card DURING resolution — the spell lands on the stack
    // immediately and no lingering (cleanup-less) permission is granted.
    // The body under test is built by the real `arm_rebound` producer, not
    // a hand-duplicated effect, so this locks the producer→router contract.
    #[test]
    fn armed_body_resolves_to_during_resolution_cast_with_no_lingering_permission() {
        use crate::game::effects::cast_from_zone;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
        use crate::types::card_type::CoreType;
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;
        use std::sync::Arc;

        let mut state = GameState::new_two_player(42);
        let owner = PlayerId(0);
        let exiled = create_object(
            &mut state,
            CardId(123),
            owner,
            "Rebound Card".to_string(),
            Zone::Exile,
        );
        // A real targetless sorcery spell ability so the during-resolution
        // cast has a face to put on the stack.
        {
            let obj = state.objects.get_mut(&exiled).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types = obj.card_types.clone();
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        }

        let mut events: Vec<GameEvent> = Vec::new();
        arm_rebound(&mut state, exiled, owner, &mut events);
        let body = state.delayed_triggers[0].ability.clone();
        cast_from_zone::resolve(&mut state, &body, &mut events)
            .expect("armed Rebound body must resolve");

        assert_eq!(
            state.objects[&exiled].zone,
            Zone::Stack,
            "CR 608.2g: the recast happens as the trigger resolves"
        );
        assert_eq!(state.stack.len(), 1);
        assert!(
            !state.objects[&exiled].casting_permissions.iter().any(|p| {
                matches!(
                    p,
                    CastingPermission::ExileWithAltCost {
                        resolution_cleanup: None,
                        ..
                    }
                )
            }),
            "no lingering exile-cast permission may be granted"
        );
    }
}
