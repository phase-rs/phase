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
const SWORDS_ORACLE: &str = "Exile target creature. Its controller gains life equal to its power.";

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
fn vren_counts_creatures_exiled_under_opponents_control_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Vren, the Relentless", 2, 2, VREN_ORACLE);

    let killed_by_shock = scenario
        .add_creature(P1, "Opponent creature killed", 1, 1)
        .id();
    let exiled_by_spell = scenario
        .add_creature(P1, "Opponent creature exiled", 2, 2)
        .id();
    let mut stolen = scenario.add_creature(P1, "Opponent-owned stolen creature", 2, 2);
    stolen.controlled_by(P0);
    let stolen = stolen.id();

    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let swords_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_ORACLE)
        .id();
    let swords_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_ORACLE)
        .id();
    let source = ObjectId(9_999);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, source, false, vec![]),
            ManaUnit::new(ManaType::White, source, false, vec![]),
            ManaUnit::new(ManaType::White, source, false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    runner.cast(shock).target_object(killed_by_shock).resolve();
    runner
        .cast(swords_one)
        .target_object(exiled_by_spell)
        .resolve();
    runner.cast(swords_two).target_object(stolen).resolve();
    for creature in [killed_by_shock, exiled_by_spell, stolen] {
        assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    }

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
    assert!(
        runner.state().stack.is_empty(),
        "Vren's end-step trigger must finish resolving"
    );
    assert_eq!(
        rat_tokens(&runner).len(),
        2,
        "count opponent-controlled creature exiles, not the creature owned by P1 but controlled by P0"
    );
}
