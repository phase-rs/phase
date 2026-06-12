//! Regression for GitHub issue #1333 — Brain in a Jar must place a charge
//! counter before the optional free-cast sub-ability filters hand spells by
//! the post-counter mana value (1 on first activation, not 0).

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRAIN_ORACLE: &str = "{1}, {T}: Put a charge counter on this artifact, then you may cast an instant or sorcery spell with mana value equal to the number of charge counters on this artifact from your hand without paying its mana cost.\n\
{3}, {T}, Remove X charge counters from this artifact: Scry X.";

fn add_colorless_mana(runner: &mut engine::game::scenario::GameRunner, amount: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

#[test]
fn brain_oracle_parse_includes_cmc_eq_charge_counters_filter() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{FilterProp, QuantityExpr, QuantityRef, TargetFilter};

    let parsed = parse_oracle_text(
        BRAIN_ORACLE,
        "Brain in a Jar",
        &[],
        &[String::from("Artifact")],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, engine::types::ability::Effect::PutCounter { .. }))
        .expect("first activated ability must be PutCounter");
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("PutCounter must chain to optional CastFromZone sub-ability");
    let Effect::CastFromZone { target, .. } = &*sub.effect else {
        panic!("sub-ability must be CastFromZone, got {:?}", sub.effect);
    };
    let TargetFilter::Typed(filter) = target else {
        panic!("CastFromZone target must be Typed filter");
    };
    assert!(
        filter.properties.iter().any(|p| matches!(
            p,
            FilterProp::Cmc {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn { .. },
                },
                ..
            }
        )),
        "parser must retain CMC == charge-counter filter on the free-cast sub-ability; properties = {:?}",
        filter.properties
    );
}

#[test]
fn brain_in_a_jar_puts_counter_before_optional_cast_filters_cmc_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let jar_id = scenario
        .add_creature(P0, "Brain in a Jar", 0, 0)
        .from_oracle_text(BRAIN_ORACLE)
        .as_artifact()
        .id();

    // CMC 1 instant (eligible after first counter) and CMC 0 instant (not eligible).
    let bolt_id = scenario
        .add_spell_to_hand(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let free_id = scenario
        .add_spell_to_hand(P0, "Free Spell", true)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    add_colorless_mana(&mut runner, 2);

    runner.activate(jar_id, 0).accept_optional().resolve();

    let jar = &runner.state().objects[&jar_id];
    assert_eq!(
        jar.counters
            .get(&CounterType::Generic("charge".to_string())),
        Some(&1),
        "Brain in a Jar must have one charge counter after resolving the put-counter clause"
    );

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            assert!(
                cards.contains(&bolt_id),
                "CMC 1 instant must be eligible after the counter is placed; cards = {cards:?}"
            );
            assert!(
                !cards.contains(&free_id),
                "CMC 0 instant must not be eligible when one charge counter is present; cards = {cards:?}"
            );
        }
        other => panic!(
            "expected EffectZoneChoice for optional free cast with CMC 1 filter, got {other:?}"
        ),
    }

    assert_eq!(runner.state().objects[&bolt_id].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&free_id].zone, Zone::Hand);
}
