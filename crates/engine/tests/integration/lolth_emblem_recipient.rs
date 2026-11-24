use crate::support::{assert_damage_dealt, shared_card_db};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// CR 606.3 + CR 606.4 + CR 510.2 + CR 603.4: Pay the printed ultimate's
// loyalty cost, then exercise the emblem with a single combat damage recipient.
#[test]
fn lolth_emblem_applies_only_the_damaged_opponents_shortfall() {
    let db = shared_card_db().expect("Lolth's printed card fixture");
    for (prior_loss, final_life) in [(0, 12), (8, 9)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let lolth = scenario.add_real_card(P0, "Lolth, Spider Queen", Zone::Battlefield, db);
        let victims = std::array::from_fn::<_, 4, _>(|_| {
            scenario.add_creature(P0, "Doomed creature", 1, 1).id()
        });
        let sweep = scenario
            .add_spell_to_hand_from_oracle(P0, "Test sweeper", false, "Destroy all creatures.")
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let prior_loss_spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Test precombat life loss",
                false,
                "Target opponent loses 8 life.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let attacker = scenario
            .add_creature_to_hand(P0, "Hasty attacker", 3, 3)
            .haste()
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&lolth].loyalty, Some(4));
        runner.cast(sweep).resolve();
        for victim in victims {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        }
        assert_eq!(runner.state().objects[&lolth].loyalty, Some(8));
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        runner.activate(lolth, 2).resolve();
        assert_eq!(runner.state().objects[&lolth].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());

        if prior_loss > 0 {
            runner.cast(prior_loss_spell).target_player(P1).resolve();
        }
        assert_eq!(runner.life(P1), 20 - prior_loss);
        runner.cast(attacker).resolve();
        assert_eq!(runner.state().objects[&attacker].zone, Zone::Battlefield);
        runner.advance_to_combat();
        let mut events = runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("attack P1")
            .events;
        for _ in 0..32 {
            if runner.state().phase == Phase::EndCombat && runner.state().stack.is_empty() {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref waiting => panic!("unexpected combat prompt: {waiting:?}"),
            };
            events.extend(runner.act(action).expect("play out combat").events);
        }
        assert_damage_dealt(&events, attacker, TargetRef::Player(P1), 3, true);
        assert_eq!(runner.state().phase, Phase::EndCombat);
        assert!(runner.state().stack.is_empty());
        assert_eq!(runner.life(P0), 20);
        assert_eq!(runner.life(P1), final_life);
    }
}
