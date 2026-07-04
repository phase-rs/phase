//! Regression for issue #4124 — Alchemy perpetual type-change (Second Little Pig).
//!
//! https://github.com/phase-rs/phase/issues/4124

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, PerpetualModification};
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SECOND_LITTLE_PIG_ORACLE: &str =
    "~ perpetually becomes a Boar Spirit with base power and toughness 4/4 and gains flying.";

#[test]
fn second_little_pig_perpetual_become_resolves_through_effect_chain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Second Little Pig".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Pig".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    runner.state_mut().all_creature_types = vec!["Pig".into(), "Boar".into(), "Spirit".into()];

    let def = parse_effect_chain(SECOND_LITTLE_PIG_ORACLE, AbilityKind::Activated);
    assert!(
        matches!(
            def.effect.as_ref(),
            engine::types::ability::Effect::ApplyPerpetual {
                modification: PerpetualModification::Become {
                    creature_subtypes,
                    power: 4,
                    toughness: 4,
                    keywords,
                },
                ..
            } if creature_subtypes == &vec!["Boar".to_string(), "Spirit".to_string()]
                && keywords == &vec![Keyword::Flying]
        ),
        "precondition: parsed Become modification, got {:?}",
        def.effect,
    );

    let ability = build_resolved_from_def(&def, id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();
    engine::game::layers::flush_layers(runner.state_mut());

    let obj = runner.state().objects.get(&id).unwrap();
    assert!(obj
        .card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Boar")));
    assert!(obj
        .card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Spirit")));
    assert!(!obj
        .card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Pig")));
    assert_eq!(obj.power, Some(4));
    assert_eq!(obj.toughness, Some(4));
    assert!(obj.has_keyword(&Keyword::Flying));
}
