//! GitHub issue #7234 — Cumulative upkeep must pay typed source-counter
//! effect costs after card-data/save-state deserialization.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ABOROTH_ORACLE: &str =
    "Cumulative upkeep—Put a -1/-1 counter on this creature. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)";

/// CR 702.24a: Card-data and saved games use the externally tagged keyword
/// form. A typed Aboroth effect cost must not be replaced by a zero-mana cost.
#[test]
fn cumulative_upkeep_typed_effect_cost_survives_deserialization() {
    let keyword: Keyword = serde_json::from_str(
        r#"{"CumulativeUpkeep":{"type":"EffectCost","effect":{"type":"PutCounter","counter_type":"M1M1","count":{"type":"Fixed","value":1},"target":{"type":"SelfRef"}}}}"#,
    )
    .expect("typed CumulativeUpkeep payload deserializes");

    assert!(matches!(
        keyword,
        Keyword::CumulativeUpkeep(AbilityCost::EffectCost { effect })
            if matches!(
                effect.as_ref(),
                Effect::PutCounter {
                    counter_type: CounterType::Minus1Minus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }
            )
    ));
}

/// CR 702.24a: Aboroth's effect-as-cost is paid once per age counter. With one
/// pre-existing age counter, the upkeep tick makes two and paying the prompt
/// must place two -1/-1 counters while keeping Aboroth on the battlefield.
#[test]
fn aboroth_cumulative_upkeep_scales_and_pays_source_counter_effect_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let aboroth = scenario
        .add_creature_from_oracle(P0, "Aboroth", 9, 9, ABOROTH_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aboroth)
        .expect("Aboroth exists")
        .counters
        .insert(CounterType::Age, 1);

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { cost, .. } => assert!(matches!(
            cost,
            AbilityCost::EffectCost {
                effect,
            } if matches!(
                effect.as_ref(),
                Effect::PutCounter {
                    counter_type: CounterType::Minus1Minus1,
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::SelfRef,
                }
            )
        )),
        other => panic!("expected Aboroth's cumulative-upkeep payment prompt, got {other:?}"),
    }

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("Aboroth's counter cost is payable");

    let aboroth_object = runner
        .state()
        .objects
        .get(&aboroth)
        .expect("Aboroth remains");
    assert_eq!(aboroth_object.zone, Zone::Battlefield);
    assert_eq!(aboroth_object.counters.get(&CounterType::Age), Some(&2));
    assert_eq!(
        aboroth_object.counters.get(&CounterType::Minus1Minus1),
        Some(&2),
        "paying the cumulative cost must place one -1/-1 counter for each age counter"
    );
}
