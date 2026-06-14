//! Regression for issue #2353: Ulamog, the Defiler must enter with +1/+1 counters
//! equal to the greatest mana value among cards in exile.
//!
//! https://github.com/phase-rs/phase/issues/2353

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const ULAMOG_FULL_ORACLE: &str = "When you cast this spell, target opponent exiles the top half of their library, rounded up.\n\
Ward—Sacrifice two permanents.\n\
Ulamog enters with a number of +1/+1 counters on it equal to the greatest mana value among cards in exile.\n\
Ulamog has annihilator X, where X is the number of +1/+1 counters on it.";

const ULAMOG_ETB_ORACLE: &str = "Ulamog enters with a number of +1/+1 counters on it equal to the greatest mana value among cards in exile.";

fn plus_one_counters(
    runner: &engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0)
}

#[test]
fn ulamog_parses_etb_counters_replacement_from_full_oracle() {
    let parsed = parse_oracle_text(
        ULAMOG_FULL_ORACLE,
        "Ulamog, the Defiler",
        &[],
        &["Creature".to_string()],
        &["Eldrazi".to_string()],
    );
    let replacement = parsed
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::Moved)
        .expect("Ulamog must parse ETB +1/+1 counter replacement");
    assert!(matches!(
        replacement.execute.as_ref().map(|e| &*e.effect),
        Some(engine::types::ability::Effect::PutCounter { .. })
    ));
}

#[test]
fn ulamog_enters_with_counters_equal_to_greatest_exiled_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let exiled = scenario
        .add_creature_to_hand(P0, "Seven Drop", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            generic: 7,
            shards: vec![],
        })
        .id();

    let ulamog = scenario
        .add_creature_to_hand_from_oracle(P0, "Ulamog, the Defiler", 10, 10, ULAMOG_ETB_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 10,
            shards: vec![],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            );
            12
        ],
    );

    let mut runner = scenario.build();
    move_to_zone(runner.state_mut(), exiled, Zone::Exile, &mut Vec::new());

    let card_id = runner.state().objects[&ulamog].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ulamog,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting Ulamog");

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            WaitingFor::Priority { .. } => break,
            _ => runner.pass_both_players(),
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&ulamog].zone,
        Zone::Battlefield,
        "Ulamog must resolve onto the battlefield"
    );
    assert_eq!(
        plus_one_counters(&runner, ulamog),
        7,
        "Ulamog must enter with +1/+1 counters equal to the greatest exiled card MV"
    );
}
