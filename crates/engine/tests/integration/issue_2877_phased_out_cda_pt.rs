//! [#2877](https://github.com/phase-rs/phase/issues/2877): regression coverage for
//! */* CDA creatures (e.g. Lumra) while phased out.
//!
//! CR 702.26b + CR 702.26e: phased-out permanents are excluded from the layer
//! pass and SBA scans — their last-computed P/T is frozen, not re-derived and
//! not reset to base */*.

use engine::game::game_object::PhaseOutCause;
use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::phasing::phase_out_object;
use engine::game::sba::check_state_based_actions;
use engine::game::zones::create_object;
use engine::types::ability::{
    ContinuousModification, ControllerRef, QuantityExpr, QuantityRef, StaticDefinition,
    TargetFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn setup_land(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        controller,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Land];
        obj.base_card_types = obj.card_types.clone();
    }
    id
}

fn land_count_pt_expr() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
        },
    }
}

fn setup_lumra_like_cda_creature(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(2),
        controller,
        "Lumra Stand-in".to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(0);
        obj.toughness = Some(0);
        obj.base_power = Some(0);
        obj.base_toughness = Some(0);

        let pt = land_count_pt_expr();
        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .cda()
            .modifications(vec![
                ContinuousModification::SetDynamicPower { value: pt.clone() },
                ContinuousModification::SetDynamicToughness { value: pt },
            ]);
        obj.static_definitions = vec![def.clone()].into();
        obj.base_static_definitions = std::sync::Arc::new(vec![def]);
    }
    id
}

#[test]
fn phased_out_star_star_cda_creature_freezes_last_computed_pt() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    for _ in 0..3 {
        setup_land(&mut state, controller);
    }
    let creature = setup_lumra_like_cda_creature(&mut state, controller);

    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);
    assert_eq!(state.objects[&creature].power, Some(3));
    assert_eq!(state.objects[&creature].toughness, Some(3));

    let mut events = Vec::new();
    phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);
    assert!(state.objects[&creature].is_phased_out());

    state.layers_dirty.mark_full();
    flush_layers(&mut state);

    assert_eq!(
        state.objects[&creature].power,
        Some(3),
        "phased-out CDA creature must keep its last-computed P/T (CR 702.26e freeze)"
    );
    assert_eq!(state.objects[&creature].toughness, Some(3));

    check_state_based_actions(&mut state, &mut events);
    assert_eq!(
        state.objects[&creature].zone,
        Zone::Battlefield,
        "phased-out creature is excluded from CR 704.5f zero-toughness SBAs"
    );
}

#[test]
fn phased_out_cda_pt_does_not_track_board_changes_while_phased_out() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    for _ in 0..3 {
        setup_land(&mut state, controller);
    }
    let creature = setup_lumra_like_cda_creature(&mut state, controller);

    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);

    let mut events = Vec::new();
    phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);

    // Board changes while phased out must not re-derive the CDA (CR 702.26e).
    setup_land(&mut state, controller);
    setup_land(&mut state, controller);

    state.layers_dirty.mark_full();
    flush_layers(&mut state);

    assert_eq!(
        state.objects[&creature].power,
        Some(3),
        "P/T must stay frozen at phase-out value, not track new lands"
    );
    assert_eq!(state.objects[&creature].toughness, Some(3));
}
