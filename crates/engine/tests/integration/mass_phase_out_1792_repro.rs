//! [#1792](https://github.com/phase-rs/phase/issues/1792): mass phase-out must mark
//! attached Equipment as indirectly phased out when its host is in the target set.

use engine::game::effects::phase_out::resolve;
use engine::game::game_object::{PhaseOutCause, PhaseStatus};
use engine::game::phasing::{execute_untap_step_phasing, phase_out_object};
use engine::game::zones::create_object;
use engine::types::ability::{ControllerRef, Effect, ResolvedAbility, TargetFilter, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn setup_creature(state: &mut GameState, name: &str, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Creature];
    }
    id
}

fn setup_equipment(
    state: &mut GameState,
    name: &str,
    controller: PlayerId,
    host: ObjectId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(2),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.attached_to = Some(host.into());
    }
    if let Some(host_obj) = state.objects.get_mut(&host) {
        host_obj.attachments.push(id);
    }
    id
}

#[test]
fn mass_phase_out_leaves_equipment_indirect_when_host_in_set() {
    let mut state = GameState::new_two_player(42);
    let creature = setup_creature(&mut state, "Bear", PlayerId(0));
    let equipment = setup_equipment(&mut state, "Bonesplitter", PlayerId(0), creature);
    let source = create_object(
        &mut state,
        CardId(9),
        PlayerId(0),
        "Teferi".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::PhaseOut {
            target: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
        },
        vec![],
        source,
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve(&mut state, &ability, &mut events).expect("mass phase out");

    assert!(matches!(
        state.objects[&creature].phase_status,
        PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly
        }
    ));
    assert!(
        matches!(
            state.objects[&equipment].phase_status,
            PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Indirectly
            }
        ),
        "equipment must be indirectly phased (CR 702.26h), got {:?}",
        state.objects[&equipment].phase_status
    );
}

#[test]
fn direct_pass_after_indirect_does_not_promote_attachment_to_direct() {
    let mut state = GameState::new_two_player(42);
    let creature = setup_creature(&mut state, "Bear", PlayerId(0));
    let equipment = setup_equipment(&mut state, "Bonesplitter", PlayerId(0), creature);
    let mut events = Vec::new();

    phase_out_object(&mut state, equipment, PhaseOutCause::Directly, &mut events);
    phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);

    assert!(matches!(
        state.objects[&equipment].phase_status,
        PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Indirectly
        }
    ));
}

#[test]
fn indirect_equipment_phases_in_with_host_on_untap() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    let creature = setup_creature(&mut state, "Bear", PlayerId(0));
    let equipment = setup_equipment(&mut state, "Bonesplitter", PlayerId(0), creature);
    let source = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Teferi".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::PhaseOut {
            target: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
        },
        vec![],
        source,
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve(&mut state, &ability, &mut events).expect("mass phase out");
    events.clear();

    execute_untap_step_phasing(&mut state, &mut events);

    assert!(state.objects[&creature].is_phased_in());
    assert!(
        state.objects[&equipment].is_phased_in(),
        "equipment must phase in with host (CR 702.26i)"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::PermanentPhasedIn { object_id } if *object_id == equipment
    )));
}
