use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VREN_ORACLE: &str = "Ward {2}\n\
If a creature an opponent controls would die, exile it instead.\n\
At the beginning of each end step, create X 1/1 black Rat creature tokens with \"This token gets +1/+1 for each other Rat you control,\" where X is the number of creatures that were exiled under your opponents' control this turn.";
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

fn rat_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.zone == Zone::Battlefield
                && object.is_token
                && object.name.eq_ignore_ascii_case("Rat")
        })
        .map(|(id, _)| *id)
        .collect()
}

#[test]
fn vren_rat_tokens_get_dynamic_power_and_toughness_from_other_rats() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Vren, the Relentless", 2, 2, VREN_ORACLE)
        .with_subtypes(vec!["Rat", "Rogue"]);
    let non_rat = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let first_opponent_creature = scenario
        .add_creature(P1, "First opponent creature", 1, 1)
        .id();
    let second_opponent_creature = scenario
        .add_creature(P1, "Second opponent creature", 1, 1)
        .id();
    let first_shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let second_shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let source = ObjectId(9_999);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, source, false, vec![]),
            ManaUnit::new(ManaType::Red, source, false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    runner
        .cast(first_shock)
        .target_object(first_opponent_creature)
        .resolve();
    runner
        .cast(second_shock)
        .target_object(second_opponent_creature)
        .resolve();
    assert_eq!(
        runner.state().objects[&first_opponent_creature].zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects[&second_opponent_creature].zone,
        Zone::Exile
    );

    for _ in 0..128 {
        if runner.state().phase == Phase::End
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            other => panic!("unexpected action while advancing to end step: {other:?}"),
        }
    }

    assert_eq!(runner.state().phase, Phase::End);
    assert!(runner.state().stack.is_empty());
    let tokens = rat_tokens(&runner);
    assert_eq!(tokens.len(), 2);
    for token in tokens {
        let token = &runner.state().objects[&token];
        assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    }
    let non_rat = &runner.state().objects[&non_rat];
    assert_eq!((non_rat.power, non_rat.toughness), (Some(2), Some(2)));
}
