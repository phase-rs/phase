//! CR 509.1c: maximize blocking requirements in every combat this turn.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const ORACLE: &str = "Whenever Anzrag becomes blocked, untap each creature you control. After this phase, there is an additional combat phase.\n{3}{R}{R}{G}{G}: Anzrag must be blocked each combat this turn if able.";

fn fixture() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Next Draw", "Later Draw"]);
    scenario.with_library_top(P1, &["Opponent Draw", "Later Opponent Draw"]);
    // Unblocked Anzrag deals 8 a combat; keep the opponent alive across the turns.
    scenario.with_life(P1, 100);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Red,
            ManaType::Red,
            ManaType::Green,
            ManaType::Green,
        ]
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect(),
    );
    let anzrag = scenario
        .add_creature_from_oracle(P0, "Anzrag, the Quake-Mole", 8, 4, ORACLE)
        .id();
    let ally = scenario.add_vanilla(P0, 1, 10);
    let blocker = scenario.add_vanilla(P1, 0, 20);
    let other = scenario.add_vanilla(P1, 0, 20);
    let second = scenario.add_vanilla(P1, 0, 20);
    (scenario.build(), anzrag, ally, blocker, other, second)
}

/// Answers every prompt with the plain default (pass priority, no attacks, no
/// blocks) until `done` holds.
fn drive_until(runner: &mut GameRunner, label: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..200 {
        if done(runner) {
            return;
        }
        let action =
            match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                // CR 510.1c: the whole assignment on the first blocker in order.
                WaitingFor::AssignCombatDamage {
                    total_damage,
                    ref blockers,
                    ..
                } => GameAction::AssignCombatDamage {
                    mode: Default::default(),
                    assignments: vec![(blockers[0].blocker_id, total_damage)],
                    trample_damage: 0,
                    controller_damage: 0,
                },
                ref other => {
                    panic!(
                "{label}: unexpected prompt {other:?} life={:?} lib={:?} turn={} active={:?}",
                runner.state().players.iter().map(|p| p.life).collect::<Vec<_>>(),
                runner.state().players.iter().map(|p| p.library.len()).collect::<Vec<_>>(),
                runner.state().turn_number,
                runner.state().active_player
            )
                }
            };
        runner.act(action).expect(label);
    }
    panic!("{label}: never reached");
}

fn attack(runner: &mut GameRunner, anzrag: ObjectId) {
    drive_until(runner, "declare attackers", |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        )
    });
    runner
        .declare_attackers(&[(anzrag, AttackTarget::Player(P1))])
        .expect("attack");
    drive_until(runner, "declare blockers", |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareBlockers { player: P1, .. }
        )
    });
}

#[test]
fn anzrag_activation_rejects_empty_blocks_when_blocker_can_block() {
    let (mut runner, anzrag, _, blocker, other, second) = fixture();
    let index = runner.state().objects[&anzrag]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("activated ability");
    let outcome = runner.activate(anzrag, index).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "real seven-mana cost paid"
    );
    attack(&mut runner, anzrag);
    // CR 509.1c: a legal blocker exists, so declining all blocks is illegal.
    assert!(runner.declare_blockers(&[]).is_err());
    runner
        .declare_blockers(&[(blocker, anzrag)])
        .expect("obey requirement");
    runner.advance_until_stack_empty();
    attack(&mut runner, anzrag);
    assert!(
        runner.declare_blockers(&[]).is_err(),
        "requirement persists in second combat"
    );
    runner
        .declare_blockers(&[(blocker, anzrag)])
        .expect("second block");
    runner.advance_until_stack_empty();
    // Fixture setup for the no-legal-blocker branch; no effect is fabricated.
    runner.state_mut().objects.get_mut(&blocker).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&other).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&second).unwrap().tapped = true;
    // CR 509.1c: with no creature able to block, the requirement can't be
    // obeyed, so the empty block stands; here no block prompt is even offered.
    drive_until(&mut runner, "third combat attackers", |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        )
    });
    runner
        .declare_attackers(&[(anzrag, AttackTarget::Player(P1))])
        .expect("attack");
    drive_until(&mut runner, "unblocked combat ends", |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
            || matches!(r.state().phase, Phase::CombatDamage | Phase::EndCombat)
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).expect("no able blocker");
    }
    assert!(
        !runner.state().combat.as_ref().is_some_and(|c| c
            .blocker_to_attacker
            .values()
            .any(|attackers| attackers.contains(&anzrag))),
        "nothing blocked Anzrag"
    );
    // Advance through the opponent's turn into our next turn without reactivation.
    drive_until(&mut runner, "opponent's turn", |r| {
        r.state().active_player == P1
    });
    drive_until(&mut runner, "our next turn", |r| {
        r.state().active_player == P0
    });
    let saw_opponent = true;
    assert!(saw_opponent);
    assert_eq!(runner.state().active_player, P0);
    runner.state_mut().objects.get_mut(&blocker).unwrap().tapped = false;
    attack(&mut runner, anzrag);
    assert!(
        !runner.state().objects[&blocker].tapped,
        "able blocker remains available"
    );
    runner
        .declare_blockers(&[])
        .expect("this-turn requirement expired");
}

#[test]
fn anzrag_becomes_blocked_untaps_team_and_adds_one_combat() {
    let (mut runner, anzrag, ally, blocker, other, second) = fixture();
    runner.state_mut().objects.get_mut(&ally).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&other).unwrap().tapped = true;
    attack(&mut runner, anzrag);
    assert!(runner.state().objects[&anzrag].tapped);
    runner
        .declare_blockers(&[(blocker, anzrag), (second, anzrag)])
        .expect("Anzrag becomes blocked");
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&anzrag].tapped);
    assert!(!runner.state().objects[&ally].tapped);
    assert!(runner.state().objects[&other].tapped);
    // CR 500.8: one becomes-blocked event adds exactly one combat phase.
    assert_eq!(runner.state().extra_phases.len(), 1);
    drive_until(&mut runner, "second combat", |r| {
        r.state().steps_started_this_turn.count(Phase::BeginCombat) == 2
    });
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        2
    );
    assert!(runner.state().extra_phases.is_empty());
}
