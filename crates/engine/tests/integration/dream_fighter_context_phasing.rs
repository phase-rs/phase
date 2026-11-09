//! Dream Fighter's full MTGJSON Oracle text binds its combat partner without targeting.
use engine::game::ability_utils::declared_targets_in_chain;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DREAM_FIGHTER: &str = "Whenever this creature blocks or becomes blocked by a creature, this creature and that creature phase out. (While they're phased out, they're treated as though they don't exist. Each one phases in before its controller untaps during their next untap step.)";

fn priority_to_combat_prompt(runner: &mut GameRunner, blockers: bool) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } if blockers => return,
            WaitingFor::DeclareAttackers { .. } if !blockers => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("unexpected combat prompt: {other:?}"),
        }
    }
    panic!("combat declaration prompt not reached");
}

#[test]
fn dream_fighter_combat_partners_phase_out_without_announced_targets() {
    for fighter_attacks in [false, true] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let fighter = scenario
            .add_creature(if fighter_attacks { P0 } else { P1 }, "Dream Fighter", 1, 1)
            .from_oracle_text(DREAM_FIGHTER)
            .id();
        let partner = scenario
            .add_creature(
                if fighter_attacks { P1 } else { P0 },
                "Combat partner",
                2,
                2,
            )
            .id();
        let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
        let (attacker, blocker) = if fighter_attacks {
            (fighter, partner)
        } else {
            (partner, fighter)
        };
        let mut runner = scenario.build();

        priority_to_combat_prompt(&mut runner, false);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .unwrap();
        priority_to_combat_prompt(&mut runner, true);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![(blocker, attacker)],
            })
            .unwrap();

        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        let triggers: Vec<_> = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == fighter)
            .collect();
        assert_eq!(
            triggers.len(),
            1,
            "actual combat must trigger Dream Fighter"
        );
        let ability = triggers[0].ability().expect("triggered ability");
        assert!(declared_targets_in_chain(ability).is_empty());
        assert!(runner.state().objects[&fighter].is_phased_in());
        assert!(runner.state().objects[&partner].is_phased_in());

        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        for id in [fighter, partner] {
            let object = &runner.state().objects[&id];
            assert_eq!(object.zone, Zone::Battlefield);
            assert!(
                object.is_phased_out(),
                "both combat partners must phase out: {id:?}"
            );
        }
        assert!(runner.state().objects[&bystander].is_phased_in());
    }
}
