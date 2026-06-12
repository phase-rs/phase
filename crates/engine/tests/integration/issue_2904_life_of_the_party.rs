//! Regression for issue #2904: Life of the Party infinite ETB loop.
//!
//! https://github.com/phase-rs/phase/issues/2904
//!
//! The ETB trigger must carry a NonToken intervening-if so token copies do not
//! re-trigger, and the follow-up clause must goad those copies permanently.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{keyword_display_name, parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    ContinuousModification, Duration, Effect, FilterProp, TargetFilter, TriggerCondition,
    TypeFilter,
};
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const LIFE_OF_THE_PARTY_ORACLE: &str = "\
First strike, trample, haste\n\
Whenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of creatures you control.\n\
When this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game.";

fn parse_life_of_the_party() -> ParsedAbilities {
    let keywords = [Keyword::FirstStrike, Keyword::Trample, Keyword::Haste];
    let keyword_names: Vec<String> = keywords.iter().map(keyword_display_name).collect();
    parse_oracle_text(
        LIFE_OF_THE_PARTY_ORACLE,
        "Life of the Party",
        &keyword_names,
        &["Creature".to_string()],
        &["Elemental".to_string()],
    )
}

fn etb_trigger(parsed: &ParsedAbilities) -> &engine::types::ability::TriggerDefinition {
    parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone && t.destination == Some(Zone::Battlefield))
        .expect("Life of the Party ETB trigger")
}

#[test]
fn life_of_the_party_parsed_etb_has_non_token_intervening_if() {
    let parsed = parse_life_of_the_party();
    let etb = etb_trigger(&parsed);
    match &etb.condition {
        Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
            filter: TargetFilter::Typed(typed),
            ..
        }) => {
            assert!(typed.properties.contains(&FilterProp::NonToken));
            assert!(typed.type_filters.contains(&TypeFilter::Permanent));
        }
        other => panic!("expected NonToken intervening-if, got {other:?}"),
    }
}

#[test]
fn life_of_the_party_parsed_etb_goads_created_tokens_permanently() {
    let parsed = parse_life_of_the_party();
    let execute = etb_trigger(&parsed).execute.as_ref().expect("execute");
    assert!(
        matches!(execute.effect.as_ref(), Effect::CopyTokenOf { .. }),
        "expected CopyTokenOf, got {:?}",
        execute.effect
    );
    let sub = execute.sub_ability.as_ref().expect("goad sub_ability");
    match sub.effect.as_ref() {
        Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } => {
            assert_eq!(*target, Some(TargetFilter::LastCreated));
            assert_eq!(*duration, Some(Duration::Permanent));
            assert!(static_abilities[0].modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::Goaded
                }
            )));
        }
        other => panic!("expected GenericEffect goad sub, got {other:?}"),
    }
}

#[test]
fn life_of_the_party_parsed_etb_has_no_unimplemented_leaks() {
    fn has_unimplemented(effect: &Effect) -> bool {
        matches!(effect, Effect::Unimplemented { .. })
    }

    let parsed = parse_life_of_the_party();
    let execute = etb_trigger(&parsed).execute.as_ref().expect("execute");
    assert!(
        !has_unimplemented(execute.effect.as_ref()),
        "primary ETB effect leaked Unimplemented: {:?}",
        execute.effect
    );
    if let Some(sub) = &execute.sub_ability {
        assert!(
            !has_unimplemented(sub.effect.as_ref()),
            "goad sub leaked Unimplemented: {:?}",
            sub.effect
        );
    }
}

#[test]
fn life_of_the_party_runtime_token_copy_does_not_retrigger_etb() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let life = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(
            &["first strike", "trample", "haste"],
            LIFE_OF_THE_PARTY_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(life).resolve();
    let state = outcome.state();

    assert!(
        state.stack.is_empty(),
        "Life of the Party ETB must resolve without token-copy ETB recursion; stack: {:?}",
        state.stack
    );
    assert_eq!(
        life_of_the_party_count(state, P0, false),
        1,
        "the original non-token Life of the Party should remain under P0"
    );
    assert_eq!(
        life_of_the_party_count(state, P1, true),
        1,
        "the opponent should create exactly one token copy"
    );
    let token_id = *state
        .battlefield
        .iter()
        .find(|id| {
            state.objects.get(id).is_some_and(|object| {
                object.name == "Life of the Party" && object.controller == P1 && object.is_token
            })
        })
        .expect("opponent token copy should exist");
    assert!(
        state.transient_continuous_effects.iter().any(|effect| {
            effect.duration == Duration::Permanent
                && effect.affected == TargetFilter::SpecificObject { id: token_id }
                && effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded
                        }
                    )
                })
        }),
        "the created token should be permanently goaded"
    );
}

fn life_of_the_party_count(
    state: &GameState,
    controller: engine::types::player::PlayerId,
    is_token: bool,
) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| {
            object.name == "Life of the Party"
                && object.controller == controller
                && object.is_token == is_token
        })
        .count()
}
