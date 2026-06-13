//! Reproduction for issue #1602 — Ancient Dragons' combat-damage roll triggers.
//!
//! Oracle (Ancient Copper Dragon):
//! > Flying
//! > Whenever this creature deals combat damage to a player, roll a d20. You
//! > create a number of Treasure tokens equal to the result.
//!
//! The report: the trigger appears not to fire / no tokens are created. This
//! test drives real combat (unblocked 6/5 flyer → 6 combat damage to P1) and
//! then resolves the resulting roll-a-d20 trigger, asserting the controller
//! ends up with a number of Treasure tokens equal to the d20 result (1..=20),
//! NOT equal to the combat damage dealt.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::run_combat;

const ANCIENT_COPPER_DRAGON_ORACLE: &str = "Flying\nWhenever this creature deals combat damage \
to a player, roll a d20. You create a number of Treasure tokens equal to the result.";

fn treasure_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.controller == player)
        .filter(|obj| obj.name.eq_ignore_ascii_case("Treasure"))
        .count()
}

#[test]
fn ancient_copper_dragon_creates_treasures_equal_to_d20_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(
            P0,
            "Ancient Copper Dragon",
            6,
            5,
            ANCIENT_COPPER_DRAGON_ORACLE,
        )
        .id();
    let mut runner = scenario.build();

    // Unblocked flyer → 6 combat damage to P1.
    run_combat(&mut runner, vec![dragon], vec![]);

    // Resolve the roll-a-d20 trigger sitting on the stack, collecting the
    // emitted events so we can read the actual d20 result.
    let mut all_events: Vec<GameEvent> = Vec::new();
    for _ in 0..30 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            match runner.act(GameAction::PassPriority) {
                Ok(result) => all_events.extend(result.events),
                Err(_) => break,
            }
        } else {
            break;
        }
    }

    let rolled = all_events.iter().find_map(|e| match e {
        GameEvent::DieRolled {
            result, sides: 20, ..
        } => result.map(usize::from),
        _ => None,
    });

    let treasures = treasure_count(&runner, P0);
    eprintln!("d20 roll = {rolled:?}, treasures created = {treasures}");

    let rolled = rolled.expect("Ancient Copper Dragon should roll a d20 on combat damage");
    assert!(
        (1..=20).contains(&rolled),
        "d20 result out of range: {rolled}"
    );
    assert_eq!(
        treasures, rolled,
        "should create Treasures equal to the d20 result ({rolled}), not the combat damage (6)"
    );
}
