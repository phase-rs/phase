//! Regression for issue #5916: a leave-the-battlefield trigger created while
//! another spell is below it must be put on the stack immediately.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RESCUER_CHWINGA_ORACLE: &str = "Flash\nNatural Shelter — When this creature enters, you may return another permanent you control to its owner's hand.";
const REVEILLARK_ORACLE: &str = "When Reveillark leaves the battlefield, return up to two target creature cards with power 2 or less from your graveyard to the battlefield.";
const ELECTROLYZE_ORACLE: &str = "Electrolyze deals 2 damage divided as you choose among one or two target creatures and/or players.\nDraw a card.";

#[test]
fn reveillark_trigger_stacks_above_unresolved_electrolyze() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reveillark = scenario
        .add_creature_from_oracle(P0, "Reveillark", 4, 3, REVEILLARK_ORACLE)
        .id();
    let rescuer_chwinga = scenario
        .add_creature_to_hand_from_oracle(P0, "Rescuer Chwinga", 2, 2, RESCUER_CHWINGA_ORACLE)
        .id();
    let electrolyze = scenario
        .add_spell_to_hand_from_oracle(P1, "Electrolyze", true, ELECTROLYZE_ORACLE)
        .id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner.cast(electrolyze).target_player(P0).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to P0");
    runner.cast(rescuer_chwinga).commit();

    let mut accepted_chwinga = false;
    for _ in 0..32 {
        if runner.state().objects[&reveillark].zone == Zone::Hand && runner.state().stack.len() == 2
        {
            break;
        }

        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority through the response stack");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order the single triggered ability");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                accepted_chwinga = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Rescuer Chwinga's return");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&reveillark),
                    "Reveillark must be selectable for Rescuer Chwinga's return"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![reveillark],
                    })
                    .expect("return Reveillark to its owner's hand");
            }
            other => panic!("unexpected state while resolving Chwinga: {other:?}"),
        }
    }

    assert!(accepted_chwinga, "reach guard: Chwinga's ETB was resolved");
    assert_eq!(runner.state().objects[&reveillark].zone, Zone::Hand);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Reveillark's trigger must be on the stack before Electrolyze resolves"
    );
    assert_eq!(runner.state().stack[0].source_id, electrolyze);
    assert_eq!(runner.state().stack[1].source_id, reveillark);
}
