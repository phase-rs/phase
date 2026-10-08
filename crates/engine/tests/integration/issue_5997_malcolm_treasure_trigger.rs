//! Issue #5997 - Malcolm, Keen-Eyed Navigator must create one Treasure for each
//! distinct opponent dealt damage by one or more Pirates you control.
//!
//! Oracle text verified from generated MTGJSON/card-data:
//! "Whenever one or more Pirates you control deal damage to your opponents, you
//! create a Treasure token for each opponent dealt damage."

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const MALCOLM_ORACLE: &str = "Flying\nWhenever one or more Pirates you control deal damage to \
your opponents, you create a Treasure token for each opponent dealt damage. (It's an artifact \
with \"{T}, Sacrifice this token: Add one mana of any color.\")\nPartner";

fn count_treasures(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.zone == Zone::Battlefield
                && obj.is_token
                && obj.name.eq_ignore_ascii_case("Treasure")
        })
        .count()
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

fn drive_until_trigger_stacked(runner: &mut GameRunner) {
    for _ in 0..64 {
        if !runner.state().stack.is_empty() {
            return;
        }

        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should advance combat");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("defender should be able to declare no blockers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("default trigger order should be accepted");
            }
            ref other => panic!("unexpected wait state while advancing combat: {other:?}"),
        }
    }
    panic!("Malcolm trigger never reached the stack");
}

#[test]
fn malcolm_creates_treasure_per_distinct_damaged_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Malcolm, Keen-Eyed Navigator", 2, 2, MALCOLM_ORACLE)
        .with_subtypes(vec!["Siren", "Pirate"]);
    let pirate_to_p1 = scenario
        .add_creature(P0, "Boarding Party A", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let first_pirate_to_p2 = scenario
        .add_creature(P0, "Boarding Party B", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let second_pirate_to_p2 = scenario
        .add_creature(P0, "Boarding Party C", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let non_pirate_to_p2 = scenario.add_creature(P0, "Deckhand", 1, 1).id();
    let mut runner = scenario.build();

    let treasures_before = count_treasures(&runner, P0);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (pirate_to_p1, AttackTarget::Player(P1)),
            (first_pirate_to_p2, AttackTarget::Player(P2)),
            (second_pirate_to_p2, AttackTarget::Player(P2)),
            (non_pirate_to_p2, AttackTarget::Player(P2)),
        ])
        .expect("all attackers should be legal");
    drive_until_trigger_stacked(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P1), 19, "P1 took one Pirate combat damage");
    assert_eq!(
        runner.life(P2),
        17,
        "P2 took two Pirate combat damage plus one non-Pirate combat damage"
    );
    assert_eq!(
        count_treasures(&runner, P0),
        treasures_before + 2,
        "Malcolm must create one Treasure per distinct damaged opponent: P1 and P2. \
         The second Pirate hit on P2 and the non-Pirate hit on P2 must not inflate the count."
    );
}
