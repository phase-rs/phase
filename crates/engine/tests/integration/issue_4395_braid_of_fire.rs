//! GitHub issue #4395 — Braid of Fire's cumulative upkeep is a mana-producing
//! effect cost, not an unsupported trigger.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, Effect, ManaContribution, ManaProduction, QuantityExpr, ResolvedAbility,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRAID_OF_FIRE_ORACLE: &str = "Cumulative upkeep—Add {R}. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)";

/// CR 702.24a + CR 106.4: After the upkeep tick creates two age counters,
/// paying Braid of Fire's cumulative cost adds two red mana rather than
/// sacrificing it. The exact Oracle pipeline guards the synthesized trigger,
/// per-counter expansion, and resolution-time effect-cost payment together.
#[test]
fn braid_of_fire_cumulative_upkeep_adds_red_for_each_age_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let braid = scenario
        .add_enchantment_from_oracle(P0, "Braid of Fire", BRAID_OF_FIRE_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&braid)
        .expect("Braid of Fire exists")
        .counters
        .insert(CounterType::Age, 1);

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { cost, .. } => assert!(matches!(
            cost,
            AbilityCost::EffectCost { effect }
                if matches!(
                    effect.as_ref(),
                    Effect::Mana {
                        produced: ManaProduction::Fixed { colors, .. },
                        target: None,
                        ..
                    } if colors == &vec![engine::types::mana::ManaColor::Red; 2]
                )
        )),
        other => panic!("expected Braid of Fire cumulative-upkeep prompt, got {other:?}"),
    }

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("Braid of Fire's mana-producing cost is payable");

    let braid_object = runner
        .state()
        .objects
        .get(&braid)
        .expect("Braid of Fire remains");
    assert_eq!(braid_object.zone, Zone::Battlefield);
    assert_eq!(braid_object.counters.get(&CounterType::Age), Some(&2));
    let pool = &runner.state().players[P0.0 as usize].mana_pool.mana;
    assert_eq!(pool.len(), 2);
    assert!(pool.iter().all(|unit| unit.color == ManaType::Red));
}

/// CR 118.3 + CR 106.4: A deterministic, untargeted fixed-mana effect cost
/// resolves through the normal unless-payment flow into the payer's mana pool.
#[test]
fn fixed_mana_effect_cost_pays_into_the_unless_payers_mana_pool() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Fixed Mana Cost Source", 1, 1).id();
    let mut runner = scenario.build();
    let pending_effect = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    runner.state_mut().waiting_for = WaitingFor::UnlessPayment {
        player: P0,
        cost: AbilityCost::EffectCost {
            effect: Box::new(Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Blue, ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }),
        },
        pending_effect: Box::new(pending_effect),
        trigger_event: None,
        effect_description: None,
        remaining: vec![],
    };

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("fixed mana effect cost is payable");

    let pool = &runner.state().players[P0.0 as usize].mana_pool.mana;
    assert_eq!(
        pool.iter().map(|unit| unit.color).collect::<Vec<_>>(),
        vec![ManaType::Blue, ManaType::Red]
    );
}
