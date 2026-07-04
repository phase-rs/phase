//! Regression for issue #941: Champions from Beyond Full Party must pump all
//! attacking creatures, not just one (or the enchantment itself).
//!
//! https://github.com/phase-rs/phase/issues/941

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const CHAMPIONS_ORACLE: &str = "When this enchantment enters, create X 1/1 colorless Hero creature tokens.\n\
Light Party — Whenever you attack with four or more creatures, scry 2, then draw a card.\n\
Full Party — Whenever you attack with eight or more creatures, those creatures get +4/+4 until end of turn.";

#[test]
fn champions_full_party_parsed_as_parent_target_pump() {
    let parsed = parse_oracle_text(CHAMPIONS_ORACLE, "Champions from Beyond", &[], &[], &[]);
    let full_party = parsed
        .triggers
        .iter()
        .find(|t| {
            t.mode == TriggerMode::YouAttack
                && matches!(
                    t.condition,
                    Some(TriggerCondition::AttackersDeclaredCount { count: 8, .. })
                )
        })
        .expect("Full Party trigger");
    let execute = full_party.execute.as_ref().expect("execute");
    match execute.effect.as_ref() {
        Effect::Pump { target, .. } => {
            assert_eq!(*target, TargetFilter::ParentTarget);
        }
        other => panic!("expected Pump ParentTarget, got {other:?}"),
    }
}

fn resolve_attack_triggers(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..count).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::ScryChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.clone(),
                    })
                    .expect("scry keep all");
            }
            other => panic!("unexpected waiting state during attack triggers: {other:?}"),
        }
    }
    panic!("attack triggers did not resolve");
}

#[test]
fn champions_full_party_pumps_all_attacking_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island", "Island"]);

    let _champions = scenario
        .add_creature(P0, "Champions from Beyond", 0, 0)
        .as_enchantment()
        .from_oracle_text(CHAMPIONS_ORACLE)
        .id();

    let mut attackers: Vec<ObjectId> = Vec::new();
    for i in 0..8 {
        attackers.push(scenario.add_vanilla(P0, 1, 1));
        let _ = i;
    }

    let mut runner = scenario.build();
    runner.advance_to_combat();

    let attack_pairs: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attack_pairs)
        .expect("declare eight attackers");

    resolve_attack_triggers(&mut runner);

    for attacker in attackers {
        let obj = runner
            .state()
            .objects
            .get(&attacker)
            .expect("attacker exists");
        let power = obj.power.unwrap_or(0);
        let toughness = obj.toughness.unwrap_or(0);
        assert_eq!(
            (power, toughness),
            (5, 5),
            "attacker {attacker:?} should be pumped +4/+4 by Full Party"
        );
    }
}
