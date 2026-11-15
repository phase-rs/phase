//! Regression for Memory Jar's delayed per-player discard.
//!
//! The delayed end-step effect is part of the same "each player" ability as
//! the immediate hand exile and draw. Its quantity and target references must
//! retain the player being iterated when the delayed trigger resolves.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, Effect, QuantityExpr, QuantityRef, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MEMORY_JAR_ORACLE: &str = "{T}, Sacrifice this artifact: Each player exiles all cards from their hand face down and draws seven cards. At the beginning of the next end step, each player discards their hand and returns to their hand each card they exiled this way.";

fn object_zone(runner: &engine::game::scenario::GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

fn graveyard_contains(
    runner: &engine::game::scenario::GameRunner,
    player: PlayerId,
    object: ObjectId,
) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .is_some_and(|candidate| candidate.graveyard.contains(&object))
}

fn assert_no_unimplemented(effect: &Effect) {
    match effect {
        Effect::Unimplemented { name, .. } => {
            panic!("Memory Jar parsed an unimplemented effect: {name}")
        }
        Effect::CreateDelayedTrigger { effect, .. } => {
            assert_no_unimplemented_ability(effect);
        }
        _ => {}
    }
}

fn assert_no_unimplemented_ability(ability: &AbilityDefinition) {
    assert_no_unimplemented(ability.effect.as_ref());
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        assert_no_unimplemented_ability(sub_ability);
    }
    if let Some(else_ability) = ability.else_ability.as_deref() {
        assert_no_unimplemented_ability(else_ability);
    }
}

fn find_delayed_ability(ability: &AbilityDefinition) -> Option<&AbilityDefinition> {
    if matches!(ability.effect.as_ref(), Effect::CreateDelayedTrigger { .. }) {
        return Some(ability);
    }
    ability
        .sub_ability
        .as_deref()
        .and_then(find_delayed_ability)
        .or_else(|| {
            ability
                .else_ability
                .as_deref()
                .and_then(find_delayed_ability)
        })
}

#[test]
fn memory_jar_delayed_discard_stays_scoped_to_each_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let jar = scenario
        .add_creature_from_oracle(P0, "Memory Jar", 0, 0, MEMORY_JAR_ORACLE)
        .as_artifact()
        .id();

    let p0_hand: Vec<_> = (0..7)
        .map(|index| scenario.add_card_to_hand(P0, &format!("P0 original {index}")))
        .collect();
    let p1_hand: Vec<_> = (0..7)
        .map(|index| scenario.add_card_to_hand(P1, &format!("P1 original {index}")))
        .collect();
    for index in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("P0 library padding {index}"));
        scenario.add_card_to_library_top(P1, &format!("P1 library padding {index}"));
    }
    let p0_draws: Vec<_> = (0..7)
        .map(|index| scenario.add_card_to_library_top(P0, &format!("P0 draw {index}")))
        .collect();
    let p1_draws: Vec<_> = (0..7)
        .map(|index| scenario.add_card_to_library_top(P1, &format!("P1 draw {index}")))
        .collect();

    let mut runner = scenario.build();
    let jar_ability = runner.state().objects[&jar].abilities[0].clone();
    assert_no_unimplemented_ability(&jar_ability);
    let delayed_ability = find_delayed_ability(&jar_ability).expect("Memory Jar delayed ability");
    let Effect::CreateDelayedTrigger {
        effect,
        uses_tracked_set: true,
        ..
    } = delayed_ability.effect.as_ref()
    else {
        unreachable!("find_delayed_ability returned a non-delayed ability");
    };
    assert!(matches!(
        effect.effect.as_ref(),
        Effect::Discard {
            count: QuantityExpr::Ref {
                qty: QuantityRef::HandSize {
                    player: engine::types::ability::PlayerScope::ScopedPlayer
                }
            },
            ..
        }
    ));
    let Some(return_effect) = effect.sub_ability.as_deref() else {
        panic!("Memory Jar delayed return");
    };
    assert!(matches!(
        return_effect.effect.as_ref(),
        Effect::ChangeZoneAll {
            origin: Some(Zone::Exile),
            destination: Zone::Hand,
            target: TargetFilter::TrackedSetFiltered {
                caused_by: Some(engine::types::ability::ThisWayCause::Exiled),
                ..
            },
            ..
        }
    ));

    runner.activate(jar, 0).pay_with(&[jar]).resolve();

    assert_eq!(runner.state().delayed_triggers.len(), 1);
    for object in p0_hand.iter().chain(p1_hand.iter()) {
        assert_eq!(object_zone(&runner, *object), Zone::Exile);
    }
    for object in p0_draws.iter().chain(p1_draws.iter()) {
        assert_eq!(object_zone(&runner, *object), Zone::Hand);
    }

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    for object in &p0_hand {
        assert_eq!(object_zone(&runner, *object), Zone::Hand);
        assert!(!graveyard_contains(&runner, P0, *object));
    }
    for object in &p1_hand {
        assert_eq!(object_zone(&runner, *object), Zone::Hand);
        assert!(!graveyard_contains(&runner, P1, *object));
    }
    for object in &p0_draws {
        assert_eq!(object_zone(&runner, *object), Zone::Graveyard);
        assert!(graveyard_contains(&runner, P0, *object));
    }
    for object in &p1_draws {
        assert_eq!(object_zone(&runner, *object), Zone::Graveyard);
        assert!(graveyard_contains(&runner, P1, *object));
    }

    assert_eq!(
        runner.state().players[0].graveyard.len(),
        p0_draws.len() + 1
    );
    assert_eq!(runner.state().players[1].graveyard.len(), p1_draws.len());
}
