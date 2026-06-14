//! Regression for issue #2357: Norin the Wary must exile itself when any creature
//! attacks, not only when a player casts a spell.
//!
//! https://github.com/phase-rs/phase/issues/2357

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const NORIN_ORACLE: &str = "When a player casts a spell or a creature attacks, exile Norin. \
Return it to the battlefield under its owner's control at the beginning of the next end step.";

fn resolve_stack_and_triggers(runner: &mut GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty()
        || matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    {
        guard += 1;
        assert!(
            guard < 128,
            "stalled resolving Norin trigger; waiting_for = {:?}, stack = {}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => break,
            WaitingFor::DeclareBlockers { .. } => break,
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while resolving Norin exile trigger");
            }
        }
    }
}

#[test]
fn norin_parses_spell_cast_and_creature_attacks_triggers() {
    let parsed = parse_oracle_text(
        NORIN_ORACLE,
        "Norin the Wary",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warrior".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "cross-subject 'or a creature attacks' must split into two triggers"
    );
    assert_eq!(parsed.triggers[0].mode, TriggerMode::SpellCast);
    assert_eq!(parsed.triggers[1].mode, TriggerMode::Attacks);
}

#[test]
fn norin_exiles_on_creature_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let norin = scenario
        .add_creature_from_oracle(P0, "Norin the Wary", 2, 1, NORIN_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&norin].zone, Zone::Battlefield);

    runner.state_mut().active_player = P1;
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
                assignments: vec![],
            })
            .expect("DeclareBlockers should succeed");
        runner.pass_both_players();
    }

    resolve_stack_and_triggers(&mut runner);

    assert_eq!(
        runner.state().objects[&norin].zone,
        Zone::Exile,
        "Norin must exile itself when a creature attacks (issue #2357)"
    );
}
