//! Tests for Dash (CR 702.109). Declared from `game/mod.rs` so `dash.rs` stays
//! implementation-only.

use super::dash::install_dash_riders;
use crate::game::keywords::has_haste;
use crate::game::layers::evaluate_layers;
use crate::game::stack::resolve_top;
use crate::game::triggers::check_delayed_triggers;
use crate::game::zones::{create_object, move_to_zone};
use crate::types::ability::{DelayedTriggerCondition, Effect, TargetFilter};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Put a creature on the battlefield under player 0 and install the Dash riders
/// on it (as the stack resolution path does for a dash-cast spell).
fn dash_creature_on_battlefield(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        PlayerId(0),
        "Dasher".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.base_power = Some(2);
        obj.toughness = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
    }
    install_dash_riders(state, id, PlayerId(0));
    id
}

/// CR 702.109a: the dash permanent has haste.
#[test]
fn install_grants_haste() {
    let mut state = GameState::new_two_player(42);
    let id = dash_creature_on_battlefield(&mut state);
    evaluate_layers(&mut state);
    assert!(
        has_haste(&state.objects[&id]),
        "a dash-cast creature must have haste"
    );
}

/// CR 702.109a: the dash permanent is scheduled to be returned to its owner's
/// hand at the beginning of the next end step (one-shot delayed trigger).
#[test]
fn install_schedules_next_end_step_return() {
    let mut state = GameState::new_two_player(42);
    let id = dash_creature_on_battlefield(&mut state);
    let dt = state
        .delayed_triggers
        .iter()
        .find(|d| d.source_id == id)
        .expect("a delayed return-to-hand trigger must be scheduled");
    assert_eq!(
        dt.condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    assert!(dt.one_shot, "the return fires once");
    assert!(
        matches!(
            &dt.ability.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Hand,
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "the delayed effect returns this permanent to its owner's hand"
    );
}

/// CR 702.109a: at the next end step the delayed trigger returns the permanent
/// to its owner's hand.
#[test]
fn end_step_return_resolves() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    let id = dash_creature_on_battlefield(&mut state);

    state.phase = Phase::End;
    let stacked =
        check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
    assert!(!stacked.is_empty(), "the end-step return must fire");
    resolve_top(&mut state, &mut Vec::new());

    assert_eq!(state.objects[&id].zone, Zone::Hand);
    assert!(
        state.players[0].hand.contains(&id),
        "returned to owner's hand"
    );
    assert!(!state.battlefield.contains(&id));
}

/// CR 702.109a + CR 400.7: if the dash permanent already left the battlefield
/// (e.g. it died) before the end step, the return-to-hand silently no-ops — the
/// `origin: Battlefield` zone change can't pull it out of the graveyard.
///
/// `DelayedTriggerCondition::AtNextPhase` keys only on the phase event + source
/// (triggers.rs), never on the source's current zone, so the trigger *always*
/// stacks — even from the graveyard. The no-op is therefore enforced by the
/// `origin` guard in `process_one_zone_move` at resolution, which is exactly
/// what this test pins: the trigger fires, resolves, and changes nothing.
#[test]
fn dead_creature_is_not_returned_from_graveyard() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    let id = dash_creature_on_battlefield(&mut state);

    // The creature dies before the end step.
    move_to_zone(&mut state, id, Zone::Graveyard, &mut Vec::new());
    assert_eq!(state.objects[&id].zone, Zone::Graveyard);

    state.phase = Phase::End;
    let stacked =
        check_delayed_triggers(&mut state, &[GameEvent::PhaseChanged { phase: Phase::End }]);
    // The delayed trigger still fires from the graveyard (it keys on the phase,
    // not the source's zone); resolving it must be a no-op via the origin guard.
    assert!(
        !stacked.is_empty(),
        "the end-step return still stacks even after the creature left the battlefield"
    );
    resolve_top(&mut state, &mut Vec::new());

    assert_eq!(
        state.objects[&id].zone,
        Zone::Graveyard,
        "a dead dash creature must stay in the graveyard, not be returned to hand"
    );
    assert!(!state.players[0].hand.contains(&id));
}
