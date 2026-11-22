//! Full-Oracle activation witness for property-only target declarations.

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;

/// CR 903.3 + CR 115.1c: both commander designation and entry timing constrain
/// the target announced for the activated ability.
///
/// The printed "Put a +1/+1 counter on it if it's a creature and a loyalty
/// counter on it if it's a planeswalker" is a pre-existing `put_counter_tail`
/// gap (FU-5). This property-only target declaration row deliberately does not
/// depend on that counter clause.
#[test]
fn forge_of_heroes_targets_only_commander_entered_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forge = scenario.add_land_from_oracle(
        P0,
        "Forge of Heroes",
        "{T}: Add {C}.\n{T}: Choose target commander that entered this turn. Put a +1/+1 counter on it if it's a creature and a loyalty counter on it if it's a planeswalker.",
    ).id();
    let recent_commander = scenario
        .add_creature(P0, "Recent Commander", 2, 2)
        .as_legendary()
        .commander()
        .with_summoning_sickness()
        .id();
    let _old_commander = scenario
        .add_creature(P1, "Older Commander", 2, 2)
        .as_legendary()
        .commander()
        .id();
    let _recent_noncommander = scenario
        .add_creature(P0, "Recent Noncommander", 2, 2)
        .with_summoning_sickness()
        .id();
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&forge]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &*ability.effect,
                engine::types::ability::Effect::TargetOnly { .. }
            )
        })
        .expect("printed commander-targeting activation");
    let ability = build_resolved_from_def(
        &runner.state().objects[&forge].abilities[ability_index],
        forge,
        P0,
    );
    let target_slots = build_target_slots(runner.state(), &ability).unwrap();
    assert_eq!(target_slots.len(), 1);
    assert_eq!(
        target_slots[0].legal_targets,
        vec![TargetRef::Object(recent_commander)]
    );
    let activation = runner
        .act(GameAction::ActivateAbility {
            source_id: forge,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // The public activation auto-assigns its only legal target.
    let announced_targets: Vec<_> = activation
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::BecomesTarget {
                target, source_id, ..
            } if *source_id == forge => Some(target.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        announced_targets,
        vec![TargetRef::Object(recent_commander)],
        "neither the older commander nor the recent noncommander is targeted"
    );
    let activation_id = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == forge)
        .expect("the declared activation is on the stack")
        .id;
    let mut events = activation.events;
    while !runner.state().stack.is_empty() {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::StackResolved { object_id } if *object_id == activation_id
    )));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}
