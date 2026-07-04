//! Regression for issue #1120: Warden of the Grove's "it endures X" must put
//! counters on the entering creature (or offer a Spirit token) using X from
//! Warden's counters, through the production endure choose-one path.
//!
//! https://github.com/phase-rs/phase/issues/1120

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const WARDEN_ORACLE: &str = "At the beginning of your end step, put a +1/+1 counter on this creature.\nWhenever another nontoken creature you control enters, it endures X, where X is the number of counters on this creature. (Put X +1/+1 counters on the creature that entered or create an X/X white Spirit creature token.)";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn plus_one_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0)
}

#[test]
fn warden_endure_counter_branch_puts_warden_counters_on_entering_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let warden = scenario
        .add_creature_from_oracle(P0, "Warden of the Grove", 2, 2, WARDEN_ORACLE)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    scenario.with_mana_pool(
        P0,
        floating_mana(1, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Green))
            .collect(),
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&warden)
        .expect("warden on battlefield")
        .counters
        .insert(CounterType::Plus1Plus1, 2);

    let card_id = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting Grizzly Bears");

    let mut chose_counter_branch = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseOneOfBranch {
                source_id,
                branches,
                ..
            } => {
                assert_eq!(
                    source_id, bear,
                    "endure prompt source must be the entering creature"
                );
                let counter_index = branches
                    .iter()
                    .position(|b| {
                        matches!(
                            &*b.effect,
                            engine::types::ability::Effect::PutCounter { .. }
                        )
                    })
                    .expect("counter branch present");
                runner
                    .act(GameAction::ChooseBranch {
                        index: counter_index,
                    })
                    .expect("choosing counter branch must succeed");
                chose_counter_branch = true;
            }
            WaitingFor::Priority { .. } => {
                if chose_counter_branch && runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert!(
        chose_counter_branch,
        "Warden ETB must surface endure choose-one prompt"
    );
    assert_eq!(
        plus_one_counters(&runner, bear),
        2,
        "entering creature must receive X counters from Warden"
    );
    assert_eq!(
        plus_one_counters(&runner, warden),
        2,
        "Warden's counters must be unchanged"
    );
}
