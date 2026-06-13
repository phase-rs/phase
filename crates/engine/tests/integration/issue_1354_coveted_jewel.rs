//! Issue #1354 — Coveted Jewel must not trigger when an opponent's attacker is
//! blocked; it fires only after blockers are declared when at least one opponent
//! creature attacked you unblocked.
//!
//! Oracle:
//! > Whenever one or more creatures an opponent controls attack you and aren't
//! > blocked, that player draws three cards and gains control of this artifact.
//! > Untap it.

use super::rules::{AttackTarget, GameRunner};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const COVETED_JEWEL_ORACLE: &str = "When this artifact enters, draw three cards.\n\
{T}: Add three mana of any one color.\n\
Whenever one or more creatures an opponent controls attack you and aren't blocked, \
that player draws three cards and gains control of this artifact. Untap it.";

fn hand_size(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn declare_attack_on_p0(
    runner: &mut GameRunner,
    attacker: engine::types::identifiers::ObjectId,
    blocker_assignments: Vec<(
        engine::types::identifiers::ObjectId,
        engine::types::identifiers::ObjectId,
    )>,
) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: blocker_assignments,
            })
            .expect("DeclareBlockers should succeed");
    }
    // CR 509.2: priority during declare blockers — triggers (including
    // `YouAttackUnblocked`) go on the stack before combat damage.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner.advance_until_stack_empty();
}

#[test]
fn coveted_jewel_does_not_trigger_when_attacker_is_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jewel = scenario
        .add_creature(P0, "Coveted Jewel", 0, 0)
        .as_artifact()
        .from_oracle_text(COVETED_JEWEL_ORACLE)
        .id();
    let blocker = scenario.add_creature(P0, "Defender", 2, 2).id();
    let attacker = scenario.add_creature(P1, "Raider", 3, 3).id();
    scenario.with_library_top(P1, &["Card A", "Card B", "Card C", "Card D"]);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let p1_hand_before = hand_size(&runner, P1);
    let jewel_controller_before = runner.state().objects[&jewel].controller;

    declare_attack_on_p0(&mut runner, attacker, vec![(blocker, attacker)]);

    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before,
        "blocked attack must not make the attacking player draw three"
    );
    assert_eq!(
        runner.state().objects[&jewel].controller,
        jewel_controller_before,
        "blocked attack must not transfer Coveted Jewel"
    );
}

#[test]
fn coveted_jewel_triggers_when_attacker_is_unblocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jewel = scenario
        .add_creature(P0, "Coveted Jewel", 0, 0)
        .as_artifact()
        .from_oracle_text(COVETED_JEWEL_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "Raider", 3, 3).id();
    scenario.with_library_top(P1, &["Card A", "Card B", "Card C", "Card D"]);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let p1_hand_before = hand_size(&runner, P1);
    runner.state_mut().objects.get_mut(&jewel).unwrap().tapped = true;

    declare_attack_on_p0(&mut runner, attacker, vec![]);

    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before + 3,
        "unblocked attack must make the attacking player draw three cards"
    );
    assert_eq!(
        runner.state().objects[&jewel].controller,
        P1,
        "unblocked attack must transfer Coveted Jewel to the attacking player"
    );
    assert!(
        !runner.state().objects[&jewel].tapped,
        "Coveted Jewel should untap when the trigger resolves"
    );
}
