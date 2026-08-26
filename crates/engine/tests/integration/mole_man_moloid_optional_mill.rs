//! Mole Man, Moloid Master — token-granted optional attack trigger.
//!
//! CR 111.3: the creating ability's quoted trigger becomes part of Moloid's
//! token text. CR 603.5: the attack trigger goes on the stack and its controller
//! chooses whether to mill when it resolves. These runtime tests prove that the
//! structural carrier recognized by the swallow audit is actually playable.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MOLE_MAN_ORACLE: &str = "You may play lands from your graveyard.\nLandfall — Whenever a land you control enters, create a 1/1 green Minion creature token named Moloid with \"Whenever this token attacks, you may mill a card.\"";

fn drive_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance the stack");
    }
    panic!("stack did not drain: {:?}", runner.state().waiting_for);
}

fn setup_attacking_moloid() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Mole Man, Moloid Master", 2, 2, MOLE_MAN_ORACLE);
    let land = scenario.add_land_to_hand(P0, "Test Land").id();
    scenario.with_library_top(P0, &["Mill Witness", "Library Survivor"]);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing a land should trigger Mole Man");
    drive_stack(&mut runner);

    let moloid = runner
        .state()
        .objects
        .values()
        .find(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.name.eq_ignore_ascii_case("Moloid")
        })
        .map(|object| object.id)
        .expect("Mole Man's landfall trigger should create Moloid");

    // Isolate the granted attack trigger in the same turn without making the
    // test depend on a full turn cycle. The token itself still came from the
    // production Mole Man trigger above.
    runner
        .state_mut()
        .objects
        .get_mut(&moloid)
        .unwrap()
        .summoning_sick = false;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(moloid, AttackTarget::Player(P1))])
        .expect("Moloid should be a legal attacker");
    runner.resolve_top();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));

    (runner, moloid)
}

#[test]
fn moloid_attack_accept_mills_one_card() {
    let (mut runner, _moloid) = setup_attacking_moloid();
    let library_before = runner.state().players[P0.0 as usize].library.len();
    let graveyard_before = runner.state().players[P0.0 as usize].graveyard.len();

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Moloid mill");
    drive_stack(&mut runner);

    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 1
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard.len(),
        graveyard_before + 1
    );
}

#[test]
fn moloid_attack_decline_does_not_mill() {
    let (mut runner, _moloid) = setup_attacking_moloid();
    let library_before = runner.state().players[P0.0 as usize].library.len();
    let graveyard_before = runner.state().players[P0.0 as usize].graveyard.len();

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline Moloid mill");
    drive_stack(&mut runner);

    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard.len(),
        graveyard_before
    );
}
