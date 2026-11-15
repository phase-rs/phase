//! Plague Wight: "Whenever this creature becomes blocked, each creature
//! blocking it gets -1/-1 until end of turn." Oracle verified against Scryfall.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const PLAGUE_WIGHT: &str =
    "Whenever this creature becomes blocked, each creature blocking it gets -1/-1 until end of turn.";

fn priority_to_declare(runner: &mut GameRunner, blockers: bool) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } if blockers => return,
            WaitingFor::DeclareAttackers { .. } if !blockers => return,
            _ => runner.act(GameAction::PassPriority).unwrap(),
        };
    }
    panic!("combat prompt not reached");
}

/// CR 509.1g + CR 509.3c: only the creatures actually blocking the source get
/// -1/-1; a non-blocking creature on either side is untouched.
#[test]
fn plague_wight_shrinks_only_its_blockers() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let wight = scenario
        .add_creature(P0, "Plague Wight", 2, 2)
        .from_oracle_text(PLAGUE_WIGHT)
        .id();
    let other_attacker = scenario.add_creature(P0, "Other Attacker", 2, 2).id();
    let ally = scenario.add_creature(P0, "Ally Bystander", 3, 3).id();
    let blocker_a = scenario.add_creature(P1, "Blocker A", 2, 4).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 3, 5).id();
    let other_blocker = scenario.add_creature(P1, "Other Blocker", 4, 6).id();
    let enemy_bystander = scenario.add_creature(P1, "Enemy Bystander", 4, 6).id();
    let mut runner = scenario.build();

    priority_to_declare(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (wight, AttackTarget::Player(P1)),
                (other_attacker, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .unwrap();
    priority_to_declare(&mut runner, true);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![
                (blocker_a, wight),
                (blocker_b, wight),
                (other_blocker, other_attacker),
            ],
        })
        .unwrap();

    // Reach guard: the becomes-blocked trigger is on the stack before resolving.
    assert!(
        runner.state().stack.iter().any(|e| e.source_id == wight),
        "Plague Wight trigger must be on the stack"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());

    let pt = |id| {
        let o = &runner.state().objects[&id];
        (o.power, o.toughness)
    };
    assert_eq!(pt(blocker_a), (Some(1), Some(3)));
    assert_eq!(pt(blocker_b), (Some(2), Some(4)));
    assert_eq!(pt(other_blocker), (Some(4), Some(6)));
    assert_eq!(pt(wight), (Some(2), Some(2)));
    assert_eq!(pt(ally), (Some(3), Some(3)));
    assert_eq!(pt(enemy_bystander), (Some(4), Some(6)));
}
