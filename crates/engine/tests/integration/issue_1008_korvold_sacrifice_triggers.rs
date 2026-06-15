//! Issue #1008 — Korvold, Fae-Cursed King must draw and get a +1/+1 counter for
//! each permanent sacrificed, not just once when multiple are sacrificed together.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KORVOLD_ORACLE: &str = "Flying\nWhenever Korvold, Fae-Cursed King attacks or when it enters the battlefield, sacrifice another permanent.\nWhenever you sacrifice a permanent, put a +1/+1 counter on Korvold and draw a card.";
const SACRIFICE_TWO: &str = "Sacrifice two creatures.";
const SACRIFICE_TWO_COST: &str = "{0}, Sacrifice two creatures: Draw a card.";

#[test]
fn korvold_triggers_once_per_spell_effect_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);

    let korvold = scenario
        .add_creature_from_oracle(P0, "Korvold, Fae-Cursed King", 4, 4, KORVOLD_ORACLE)
        .id();
    let fodder_a = scenario.add_creature(P0, "Fodder A", 1, 1).id();
    let fodder_b = scenario.add_creature(P0, "Fodder B", 1, 1).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Slaughter", false, SACRIFICE_TWO)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).commit();
    let hand_before = runner.state().players[0].hand.len();

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder_a, fodder_b],
                    })
                    .expect("sacrifice two creatures");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}");
            }
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }

    let counters = runner
        .state()
        .objects
        .get(&korvold)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);

    assert_eq!(
        counters, 2,
        "Korvold should get a +1/+1 counter per sacrifice"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 2,
        "Korvold should draw once per sacrifice"
    );
}

#[test]
fn korvold_triggers_once_per_cost_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);

    let korvold = scenario
        .add_creature_from_oracle(P0, "Korvold, Fae-Cursed King", 4, 4, KORVOLD_ORACLE)
        .id();
    let source = scenario
        .add_creature_from_oracle(P0, "Costly Ritualist", 1, 1, SACRIFICE_TWO_COST)
        .id();
    let fodder_a = scenario.add_creature(P0, "Fodder A", 1, 1).id();
    let fodder_b = scenario.add_creature(P0, "Fodder B", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce activated ability");
    let hand_before = runner.state().players[0].hand.len();

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                count,
                ..
            } => {
                assert_eq!(count, 2);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder_a, fodder_b],
                    })
                    .expect("pay sacrifice cost");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}");
            }
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }

    assert_eq!(runner.state().objects[&fodder_a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&fodder_b].zone, Zone::Graveyard);

    let counters = runner
        .state()
        .objects
        .get(&korvold)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);

    assert_eq!(
        counters,
        2,
        "Korvold must trigger for each permanent sacrificed to pay a cost; waiting_for={:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "two Korvold draws plus the activated ability draw"
    );
}
