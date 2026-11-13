//! Regression for issue #3862: Ulvenwald Tracker must make two chosen creatures
//! fight each other, not fight the Tracker itself.
//!
//! https://github.com/phase-rs/phase/issues/3862

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

use super::support::{assert_damage_dealt, assert_no_damage_to};

const ULVENWALD_TRACKER_ORACLE: &str =
    "{1}{G}, {T}: Target creature you control fights another target creature.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn ulvenwald_tracker_dual_target_fight_does_not_include_tracker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tracker = scenario
        .add_creature_from_oracle(P0, "Ulvenwald Tracker", 1, 1, ULVENWALD_TRACKER_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let wolf = scenario.add_creature(P1, "Wolf", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        floating_mana(1, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Green))
            .collect(),
    );

    let mut runner = scenario.build();

    let outcome = runner
        .activate(tracker, 0)
        .target_objects(&[bear, wolf])
        .resolve();

    assert_damage_dealt(outcome.events(), bear, TargetRef::Object(wolf), 3, false);
    assert_eq!(outcome.zone_of(wolf), engine::types::zones::Zone::Graveyard);
    assert_damage_dealt(outcome.events(), wolf, TargetRef::Object(bear), 2, false);
    assert_eq!(
        outcome.state().objects[&bear].damage_marked,
        2,
        "Wolf (2 power) must deal 2 damage to Bear"
    );
    assert_eq!(
        outcome.zone_of(bear),
        engine::types::zones::Zone::Battlefield,
    );
    assert_eq!(
        outcome.state().objects[&tracker].damage_marked,
        0,
        "Ulvenwald Tracker is not a fighter in this fight"
    );
    assert_eq!(
        outcome.zone_of(tracker),
        engine::types::zones::Zone::Battlefield
    );
    assert_no_damage_to(outcome.events(), TargetRef::Object(tracker));
}

#[test]
fn ulvenwald_tracker_dual_target_fight_no_fallback_when_one_fighter_leaves() {
    use engine::game::zones::move_to_zone;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::zones::Zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let tracker = scenario
        .add_creature_from_oracle(P0, "Ulvenwald Tracker", 1, 1, ULVENWALD_TRACKER_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let wolf = scenario.add_creature(P1, "Wolf", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        floating_mana(1, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Green))
            .collect(),
    );

    let mut runner = scenario.build();
    let mut remaining_objects = vec![bear, wolf];

    runner
        .act(GameAction::ActivateAbility {
            source_id: tracker,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Ulvenwald Tracker");

    let mut events = Vec::new();
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = remaining_objects
                    .iter()
                    .position(|&o| {
                        slot.legal_targets
                            .contains(&engine::types::ability::TargetRef::Object(o))
                    })
                    .map(|pos| {
                        engine::types::ability::TargetRef::Object(remaining_objects.remove(pos))
                    })
                    .expect("fighter target for slot");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(choice),
                    })
                    .expect("choose fighter");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay activation cost");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                move_to_zone(runner.state_mut(), wolf, Zone::Graveyard, &mut events);
                runner.act(GameAction::PassPriority).expect("resolve stack");
                break;
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating fight: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "Bear must not fight when the other chosen fighter left the battlefield"
    );
    assert_eq!(
        runner.state().objects[&tracker].damage_marked,
        0,
        "Tracker must not become the fallback fighter"
    );
}
