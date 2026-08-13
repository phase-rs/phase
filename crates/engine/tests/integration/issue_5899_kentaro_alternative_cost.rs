//! CR 107.3c + CR 118.9 + CR 601.2b regression for Kentaro, the Smiling Cat.
//! Its alternative-cost X is the matching Samurai spell's mana value, not a
//! second player-chosen X.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const KENTARO_ORACLE: &str = "You may pay {X} rather than pay the mana cost for Samurai spells you cast, where X is that spell's mana value.";

#[test]
fn kentaro_offers_and_pays_the_matching_samurai_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Kentaro, the Smiling Cat", 2, 2, KENTARO_ORACLE);
    let samurai = scenario
        .add_creature_to_hand(P0, "Test Samurai", 3, 3, "")
        .with_subtypes(vec!["Samurai"])
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, samurai, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&samurai].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: samurai,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Kentaro must offer a castable alternative cost for a Samurai");

    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice { cost, .. } => assert!(matches!(
            cost,
            AdditionalCost::Choice(
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards,
                        generic: 3,
                    },
                },
                _
            ) if shards.is_empty()
        )),
        other => panic!("expected Kentaro alternative-cost choice, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("the mana-value alternative cost must be payable");

    assert!(
        !runner.state().stack.is_empty(),
        "accepting Kentaro's alternative must complete casting without another X prompt"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .mana
            .is_empty(),
        "the alternative cost must consume the Samurai spell's mana value"
    );
}
