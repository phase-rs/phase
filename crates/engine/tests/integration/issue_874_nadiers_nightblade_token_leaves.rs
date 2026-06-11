//! Issue #874 — Nadier's Nightblade must trigger when a creature token its
//! controller controls leaves the battlefield, even though state-based actions
//! remove the dead token from the game (CR 111.7) before trigger filters are
//! evaluated. CR 603.10a: leaves-the-battlefield abilities look back in time,
//! so the trigger's "token you control" filter must match against the
//! zone-change record snapshot, not the (already-removed) live object.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const NIGHTBLADE_ORACLE: &str = "\
Whenever a token you control leaves the battlefield, each opponent loses 1 life \
and you gain 1 life.";

fn drain_to_priority(runner: &mut GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded bound; waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

fn destroy_with_lethal_damage(runner: &mut GameRunner, object_id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .unwrap()
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(runner);
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

#[test]
fn nightblade_triggers_when_creature_token_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Nadier's Nightblade", 2, 3, NIGHTBLADE_ORACLE);
    let token_id = scenario.add_creature(P0, "Soldier", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .is_token = true;

    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    destroy_with_lethal_damage(&mut runner, token_id);

    // CR 111.7: the dead token must have been cleaned up by SBAs — this is the
    // precondition that broke live-object filter matching.
    assert!(
        !runner.state().objects.contains_key(&token_id),
        "dead token must cease to exist before the trigger resolves"
    );
    assert_eq!(
        life(&runner, P1),
        p1_life - 1,
        "each opponent must lose 1 life when a controlled token leaves the battlefield"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life + 1,
        "Nightblade's controller must gain 1 life"
    );
}

/// A nontoken creature dying must NOT satisfy the "token you control" filter.
#[test]
fn nightblade_ignores_nontoken_creature_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Nadier's Nightblade", 2, 3, NIGHTBLADE_ORACLE);
    let bear_id = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    destroy_with_lethal_damage(&mut runner, bear_id);

    assert_eq!(
        life(&runner, P1),
        p1_life,
        "nontoken death must not trigger Nightblade"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life,
        "no life gain for nontoken death"
    );
}

/// An opponent's token leaving must NOT satisfy "a token YOU control".
#[test]
fn nightblade_ignores_opponent_token_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Nadier's Nightblade", 2, 3, NIGHTBLADE_ORACLE);
    let token_id = scenario.add_creature(P1, "Soldier", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .is_token = true;

    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    destroy_with_lethal_damage(&mut runner, token_id);

    assert_eq!(
        life(&runner, P1),
        p1_life,
        "an opponent's token leaving must not trigger Nightblade"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life,
        "no life gain when an opponent's token leaves"
    );
}
