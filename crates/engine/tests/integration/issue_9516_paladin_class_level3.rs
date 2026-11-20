//! Issue #9516 — Paladin Class level 3: "+1/+1 for each other attacking
//! creature" is fixed at resolution (CR 608.2h).
//!
//! https://github.com/phase-rs/phase/issues/9516

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PALADIN_CLASS: &str = "(Gain the next level as a sorcery to add its ability.)
Spells your opponents cast during your turn cost {1} more to cast.
{2}{W}: Level 2
Creatures you control get +1/+1.
{4}{W}: Level 3
Whenever you attack, until end of turn, target attacking creature gets +1/+1 for each other attacking creature and gains double strike.";

const MURDER: &str = "Destroy target creature.";

/// CR 608.2h: Paladin Class level 3's "+1/+1 for each other attacking
/// creature" is counted once, at resolution; a later change to the attacking
/// set does not move it.
#[test]
fn paladin_class_level_three_bonus_is_latched_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let class = scenario
        .add_enchantment_from_oracle(P0, "Paladin Class", PALADIN_CLASS)
        .with_subtypes(vec!["Class"])
        .id();
    let a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let b = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let c = scenario.add_creature(P0, "Attacker C", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&class)
        .unwrap()
        .class_level = Some(3);

    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (a, AttackTarget::Player(P1)),
                (b, AttackTarget::Player(P1)),
                (c, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .expect("attack with A, B, C");
    let mut targeted = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(a)],
                    })
                    .expect("target A");
                targeted = true;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(targeted, "reach guard: the level-3 trigger must target");
    // Level 2 anthem (+1/+1) plus level 3 (+2/+2 for two other attackers).
    let power_after_trigger = runner.state().objects[&a].power;
    assert_eq!(
        power_after_trigger,
        Some(5),
        "2 base +1 anthem +2 other attackers"
    );

    runner.cast(murder).target_object(b).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&a].power,
        power_after_trigger,
        "CR 608.2h: the bonus was locked in at resolution"
    );
}
