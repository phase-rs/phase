//! Regression for GitHub issue #1322: Murktide Regent must accept Delve payment
//! and enter with +1/+1 counters for instant/sorcery cards exiled with it.
//!
//! https://github.com/phase-rs/phase/issues/1322

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ConvokeMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MURKTIDE_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
Flying\n\
This creature enters with a +1/+1 counter on it for each instant and sorcery card exiled with it.\n\
Whenever an instant or sorcery card leaves your graveyard, put a +1/+1 counter on this creature.";

#[test]
fn issue_1322_murktide_enters_with_counters_for_instants_delved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let murktide = scenario
        .add_creature_to_hand_from_oracle(P0, "Murktide Regent", 3, 3, MURKTIDE_ORACLE)
        .id();
    let instant_a = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();
    let instant_b = scenario.add_spell_to_graveyard(P0, "Shock", true).id();

    let mut runner = scenario.build();
    for _ in 0..5 {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Blue,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ));
    }

    let card_id = runner.state().objects[&murktide].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: murktide,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting Murktide");

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Delve),
                ..
            } => {
                for gy_id in [instant_a, instant_b] {
                    if runner.state().objects[&gy_id].zone == Zone::Graveyard {
                        runner
                            .act(GameAction::TapForConvoke {
                                object_id: gy_id,
                                mana_type: ManaType::Colorless,
                            })
                            .expect("delve graveyard card");
                    }
                }
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("confirm mana payment");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).ok();
            }
            _ => runner.pass_both_players(),
        }
        if runner
            .state()
            .objects
            .get(&murktide)
            .is_some_and(|o| o.zone == Zone::Battlefield)
        {
            break;
        }
    }

    runner.advance_until_stack_empty();

    let regent = runner
        .state()
        .objects
        .get(&murktide)
        .expect("Murktide should resolve to battlefield");
    assert_eq!(regent.zone, Zone::Battlefield);
    assert_eq!(
        regent
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "Murktide must enter with a +1/+1 counter per instant/sorcery delved"
    );
}
