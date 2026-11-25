use crate::support::{assert_damage_dealt, shared_card_db};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LOLTH: &str = r#"Whenever a creature you control dies, put a loyalty counter on Lolth.
[0]: You draw a card and you lose 1 life.
[−3]: Create two 2/1 black Spider creature tokens with reach and menace.
[−8]: You get an emblem with "Whenever an opponent is dealt combat damage by one or more creatures you control, if that player lost less than 8 life this turn, they lose life equal to the difference.""#;

fn tap_lands(runner: &mut GameRunner, lands: &[ObjectId]) {
    for &source_id in lands {
        runner
            .act(GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activate the basic land's mana ability");
        assert!(runner.state().objects[&source_id].tapped);
    }
}

// CR 606.3 + CR 606.4 + CR 510.2 + CR 603.4: Pay the printed ultimate's
// loyalty cost, then exercise the emblem with a single combat damage recipient.
#[test]
fn lolth_emblem_applies_only_the_damaged_opponents_shortfall() {
    let db = shared_card_db().expect("Lolth's printed card fixture");
    for (prior_loss, final_life) in [(0, 12), (8, 9)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let lolth = scenario
            .add_planeswalker_from_oracle(P0, "Lolth, Spider Queen", "Lolth", 4, LOLTH)
            .id();
        let victims = std::array::from_fn::<_, 4, _>(|_| scenario.add_vanilla(P0, 1, 1));
        let sweep = scenario.add_real_card(P0, "Wrath of God", Zone::Hand, db);
        let prior_loss_spell = scenario.add_real_card(P0, "Exsanguinate", Zone::Hand, db);
        let attacker = scenario.add_real_card(P0, "Boggart Ram-Gang", Zone::Hand, db);
        let plains =
            std::array::from_fn::<_, 4, _>(|_| scenario.add_basic_land(P0, ManaColor::White));
        let swamps: Vec<_> = (0..if prior_loss > 0 { 10 } else { 0 })
            .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
            .collect();
        let forests =
            std::array::from_fn::<_, 3, _>(|_| scenario.add_basic_land(P0, ManaColor::Green));
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&lolth].loyalty, Some(4));
        tap_lands(&mut runner, &plains);
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
            tap_lands(&mut runner, &swamps);
            runner.cast(prior_loss_spell).x(prior_loss as u32).resolve();
        }
        assert_eq!(runner.life(P1), 20 - prior_loss);
        tap_lands(&mut runner, &forests);
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
        assert_eq!(runner.life(P0), 20 + prior_loss);
        assert_eq!(runner.life(P1), final_life);
    }
}
