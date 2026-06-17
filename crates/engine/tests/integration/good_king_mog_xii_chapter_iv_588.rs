//! Regression for GitHub issue #588 — Summon: Good King Mog XII, chapter IV.
//!
//! Oracle (chapter IV): "Put two +1/+1 counters on each other Moogle you control."
//!
//! Reported bug: counters landed on every permanent on the board — opponents'
//! creatures and lands included — instead of only other Moogles you control.
//!
//! Root cause (parser): "Moogle" was absent from the curated SUBTYPES list, so
//! `parse_subtype` failed and the target filter collapsed to Typed{[],
//! controller: None, [Another]} = every other permanent.
//!
//! This test drives the full parse → `resolve_ability_chain` pipeline with a
//! battlefield matching the Scions & Spellcraft precon report: Mog (source),
//! allied Moogles, a non-Moogle creature, an opponent Moogle, and a land.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ControllerRef, Effect, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const CHAPTER_IV: &str = "Put two +1/+1 counters on each other Moogle you control.";

fn typed_leg(filter: &TargetFilter) -> Option<&TypedFilter> {
    match filter {
        TargetFilter::Typed(tf) => Some(tf),
        _ => None,
    }
}

#[test]
fn good_king_mog_chapter_iv_parses_to_scoped_put_counter_all() {
    let def = parse_effect_chain(CHAPTER_IV, AbilityKind::Spell);
    let Effect::PutCounterAll {
        counter_type,
        count,
        target,
    } = &*def.effect
    else {
        panic!("expected PutCounterAll, got {:?}", def.effect);
    };
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    assert_eq!(
        *count,
        engine::types::ability::QuantityExpr::Fixed { value: 2 }
    );
    let tf = typed_leg(target).expect("chapter IV target must be Typed");
    assert!(
        tf.type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Subtype(s) if s == "Moogle")),
        "Moogle subtype must survive lowering, got {:?}",
        tf.type_filters
    );
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(tf.properties.contains(&FilterProp::Another));
}

#[test]
fn good_king_mog_chapter_iv_counters_only_other_moogles_you_control() {
    let execute = parse_effect_chain(CHAPTER_IV, AbilityKind::Spell);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mog = scenario
        .add_creature(P0, "Good King Mog XII", 4, 4)
        .with_subtypes(vec!["Moogle", "Saga"])
        .id();

    let ally_moogle = scenario
        .add_creature(P0, "Moogle Ally", 1, 2)
        .with_subtypes(vec!["Moogle"])
        .id();

    let non_moogle = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let opp_moogle = scenario
        .add_creature(P1, "Opponent Moogle", 1, 2)
        .with_subtypes(vec!["Moogle"])
        .id();

    let land = scenario.add_basic_land(P0, ManaColor::White);

    let mut runner = scenario.build();
    let resolved = build_resolved_from_def(&execute, mog, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter IV counter placement resolves");

    let state = runner.state();
    assert_eq!(
        state.objects[&ally_moogle]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "other Moogle you control receives two +1/+1 counters"
    );
    assert!(
        !state.objects[&mog]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "Good King Mog (source) excluded by Another"
    );
    assert!(
        !state.objects[&non_moogle]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "non-Moogle creature you control must not receive counters"
    );
    assert!(
        !state.objects[&opp_moogle]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "opponent Moogle must not receive counters"
    );
    assert!(
        !state.objects[&land]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "land must not receive counters"
    );
}
