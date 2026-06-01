//! Regression for issue #709: Marchesa (Dethrone), Gisa Glorious Resurrector,
//! Uncivil Unrest — keywords/replacements/triggers reported not working.

use engine::parser::oracle::{keyword_display_name, parse_oracle_text};
use engine::types::ability::{ContinuousModification, DamageModification, Effect, TargetFilter};
use engine::types::keywords::Keyword;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;

fn parse_card(
    oracle_text: &str,
    card_name: &str,
    keywords: &[Keyword],
    types: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let keyword_names: Vec<String> = keywords.iter().map(keyword_display_name).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle_text, card_name, &keyword_names, &types, &[])
}

fn effect_is_unimplemented(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { .. })
}

#[test]
fn gisa_glorious_resurrector_parses_fully() {
    let oracle = concat!(
        "If a creature an opponent controls would die, exile it instead.\n",
        "At the beginning of your upkeep, put all creature cards exiled with Gisa onto the battlefield under your control. They gain decayed."
    );
    let parsed = parse_card(oracle, "Gisa, Glorious Resurrector", &[], &["Creature"]);
    assert!(
        parsed.replacements.iter().any(|r| r.execute.is_some()),
        "expected die-exile replacement, got replacements: {:?}",
        parsed.replacements
    );
    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| {
            t.execute
                .as_ref()
                .is_some_and(|e| !effect_is_unimplemented(&e.effect))
        })
        .expect("expected implemented upkeep trigger");
    let execute = upkeep.execute.as_ref().expect("execute");
    assert!(
        effect_references_exiled_by_source(&execute.effect),
        "Gisa upkeep must use ExiledBySource linkage; effect: {:?}",
        execute.effect
    );
}

fn effect_references_exiled_by_source(effect: &Effect) -> bool {
    match effect {
        Effect::ChangeZoneAll { target, .. } | Effect::ChangeZone { target, .. } => {
            target_uses_exiled_by_source(target)
        }
        Effect::ChooseOneOf { branches, .. } => branches
            .iter()
            .any(|b| effect_references_exiled_by_source(&b.effect)),
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|s| {
            s.affected
                .as_ref()
                .is_some_and(target_uses_exiled_by_source)
        }),
        _ => false,
    }
}

fn target_uses_exiled_by_source(target: &TargetFilter) -> bool {
    match target {
        TargetFilter::ExiledBySource => true,
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(target_uses_exiled_by_source)
        }
        _ => false,
    }
}

#[test]
fn uncivil_unrest_riot_and_double_damage_parse() {
    let oracle = concat!(
        "Nontoken creatures you control have riot.\n",
        "If a creature you control with a +1/+1 counter on it would deal damage to a permanent or player, it deals double that damage instead."
    );
    let parsed = parse_card(oracle, "Uncivil Unrest", &[], &["Enchantment"]);
    let riot_static = parsed
        .statics
        .iter()
        .find(|s| s.mode == StaticMode::Continuous)
        .expect("expected continuous static for riot grant");
    assert!(
        riot_static.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Riot
            }
        )),
        "expected riot keyword grant, got {:?}",
        riot_static.modifications
    );
    assert!(
        parsed
            .replacements
            .iter()
            .any(|r| r.damage_modification == Some(DamageModification::Double)),
        "expected double-damage replacement, got {:?}",
        parsed.replacements
    );
}

#[test]
fn marchesa_dethrone_keyword_synthesizes_attack_trigger() {
    use engine::database::synthesis::synthesize_all;
    use engine::types::card::CardFace;
    use engine::types::card_type::CoreType;

    let mut face = CardFace {
        name: "Marchesa, the Black Rose".to_string(),
        keywords: vec![Keyword::Dethrone],
        ..CardFace::default()
    };
    face.card_type.core_types.push(CoreType::Creature);
    synthesize_all(&mut face);
    assert!(
        face.triggers
            .iter()
            .any(|t| { matches!(t.mode, TriggerMode::Attacks) && t.condition.is_some() }),
        "Dethrone should add Attacks trigger with life-total condition; triggers: {:?}",
        face.triggers
    );
}

#[test]
fn uncivil_unrest_granted_riot_prompts_on_creature_etb() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::phase::Phase;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(
            P0,
            "Uncivil Unrest",
            0,
            0,
            "Nontoken creatures you control have riot.",
        )
        .as_enchantment();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bear", 2, 2)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let bear_card_id = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ),
        "granted Riot should prompt on ETB; waiting_for={:?}",
        runner.state().waiting_for
    );
}
