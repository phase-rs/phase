//! Regression for Colfenor, the Last Yew's relative-toughness dies trigger.
//!
//! CR 603.10a + CR 608.2h: the trigger compares graveyard candidates with the
//! dying creature's last-known toughness, not the departed object's reset
//! graveyard characteristics. CR 208.1: the first printed characteristic is
//! power and the second is toughness.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COLFENOR_ORACLE: &str =
    "Reach, vigilance\nWhenever Colfenor or another creature you control dies, return up to one other target creature card with lesser toughness from your graveyard to your hand.";

fn resolve_colfenor_trigger(
    runner: &mut GameRunner,
    expected_target: ObjectId,
    forbidden_target: ObjectId,
) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Colfenor's trigger");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets
                        .contains(&TargetRef::Object(expected_target)),
                    "the strict-lesser card must be a legal target: {slot:?}"
                );
                assert!(
                    !slot
                        .legal_targets
                        .contains(&TargetRef::Object(forbidden_target)),
                    "the creature whose death caused the trigger must be excluded: {slot:?}"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(expected_target)),
                    })
                    .expect("choose Colfenor's graveyard target");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while Colfenor resolves");
            }
            other => panic!("unexpected Colfenor waiting state: {other:?}"),
        }
    }
    panic!("Colfenor's trigger did not finish resolving");
}

#[test]
fn colfenor_returns_strictly_lesser_card_and_excludes_dying_trigger_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let colfenor = scenario
        .add_creature_from_oracle(P0, "Colfenor, the Last Yew", 3, 4, COLFENOR_ORACLE)
        .id();
    let dying = scenario
        .add_creature(P0, "Dying Candidate", 2, 3)
        .with_plus_counters(1)
        .id();
    let strict_lesser = scenario
        .add_creature_to_graveyard(P0, "Strict Lesser", 2, 3)
        .id();
    let equal_toughness = scenario
        .add_creature_to_graveyard(P0, "Equal Toughness", 2, 4)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&dying)
        .expect("dying creature exists")
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);

    assert_eq!(runner.state().objects[&dying].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&colfenor].zone, Zone::Battlefield);
    resolve_colfenor_trigger(&mut runner, strict_lesser, dying);

    assert_eq!(
        runner.state().objects[&strict_lesser].zone,
        Zone::Hand,
        "a card with toughness strictly less than Colfenor's LKI toughness returns"
    );
    assert_eq!(
        runner.state().objects[&equal_toughness].zone,
        Zone::Graveyard,
        "a card with equal toughness is not legal for 'lesser toughness'"
    );
    assert_eq!(
        runner.state().objects[&dying].zone,
        Zone::Graveyard,
        "the card that caused the trigger cannot be chosen as 'another'"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must finish at an empty-stack priority boundary"
    );
}
