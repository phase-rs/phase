//! Regression for issue #2904: Life of the Party infinite ETB loop.
//!
//! https://github.com/phase-rs/phase/issues/2904
//!
//! The ETB trigger must carry a NonToken intervening-if so token copies do not
//! re-trigger, and the follow-up clause must goad those copies permanently.

use engine::game::combat::{
    attacker_constraints_for_active_player, get_valid_attacker_ids, CombatRequirement,
};
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{keyword_display_name, parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    ContinuousModification, Duration, Effect, FilterProp, ResolvedAbility, StaticDefinition,
    TargetFilter, TriggerCondition, TypeFilter, TypedFilter,
};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const LIFE_OF_THE_PARTY_ORACLE: &str = "\
First strike, trample, haste\n\
Whenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of creatures you control.\n\
When this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";

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
            end_cost: _,
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

#[test]
fn life_token_stays_goaded_after_humility_removes_its_abilities() {
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
    let humility = scenario
        .add_spell_to_hand(P0, "Humility", false)
        .as_enchantment()
        .from_oracle_text("All creatures lose all abilities and have base power and toughness 1/1.")
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(life).resolve();
    let state = outcome.state();
    assert!(state.stack.is_empty());
    assert_eq!(life_of_the_party_count(state, P0, false), 1);
    assert_eq!(life_of_the_party_count(state, P1, true), 1);
    let token_id = *state
        .battlefield
        .iter()
        .find(|id| state.objects[id].name == "Life of the Party" && state.objects[id].is_token)
        .expect("real ETB must produce the opponent's token");
    assert_eq!(state.objects[&token_id].controller, P1);
    assert!(state.objects[&token_id]
        .keywords
        .contains(&Keyword::FirstStrike));
    assert!(state.transient_continuous_effects.iter().any(|effect| {
        effect.controller == P0
            && effect.duration == Duration::Permanent
            && effect.affected == TargetFilter::SpecificObject { id: token_id }
            && effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded
                    }
                )
            })
    }));

    runner.cast(humility).resolve();
    let state = runner.state();
    assert!(!state.objects[&token_id]
        .keywords
        .contains(&Keyword::FirstStrike));
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        state,
        token_id,
        &goaded,
        &FilterContext::neutral()
    ));

    // The token is eligible on its controller's later turn. Its printed
    // abilities remain suppressed while the designation still requires attack.
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .summoning_sick = false;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert_eq!(
        constraints.get(&token_id),
        Some(&CombatRequirement::MustAttack {
            defenders: vec![],
            sources: vec![]
        })
    );
    assert!(runner.declare_attackers(&[]).is_err());
    runner
        .declare_attackers(&[(token_id, AttackTarget::Player(P0))])
        .expect("the goaded token can attack the only opponent");
}

#[test]
fn departed_life_token_does_not_designate_fresh_soldier_tokens() {
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
    let alarm = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(life).resolve();
    assert!(runner.state().stack.is_empty());
    let old_token = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            let object = &runner.state().objects[id];
            object.name == "Life of the Party" && object.is_token && object.controller == P1
        })
        .expect("the real Life trigger creates an opponent token");
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.controller == P0
                && effect.duration == Duration::Permanent
                && effect.affected == TargetFilter::SpecificObject { id: old_token }
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded,
                    })
        }));
    assert!(matches_target_filter(
        runner.state(),
        old_token,
        &goaded,
        &FilterContext::neutral()
    ));

    engine::game::zones::move_to_zone(
        runner.state_mut(),
        old_token,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    assert!(!runner.state().battlefield.contains(&old_token));
    assert!(!runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| { effect.affected == TargetFilter::SpecificObject { id: old_token } }));

    runner.cast(alarm).resolve();
    assert!(runner.state().stack.is_empty());
    let fresh = &runner.state().last_created_token_ids;
    assert_eq!(
        fresh.len(),
        2,
        "the separate resolved spell must create fresh tokens"
    );
    for &id in fresh {
        assert_ne!(id, old_token);
        assert!(runner.state().battlefield.contains(&id));
        assert!(runner.state().objects[&id].is_token);
        assert!(!matches_target_filter(
            runner.state(),
            id,
            &goaded,
            &FilterContext::neutral()
        ));
    }
}

#[test]
fn empty_last_created_goad_registration_preserves_real_life_designation() {
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
    runner.cast(life).resolve();
    let token = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            let object = &runner.state().objects[id];
            object.name == "Life of the Party" && object.is_token && object.controller == P1
        })
        .expect("the real Life trigger creates an opponent token");
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.controller == P0
                && effect.duration == Duration::Permanent
                && effect.affected == TargetFilter::SpecificObject { id: token }
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded,
                    })
        }));

    let state = runner.state_mut();
    state.last_created_token_ids.clear();
    let before = state.transient_continuous_effects.len();
    let ability = ResolvedAbility::new(
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::LastCreated)
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::Goaded,
                }])],
            duration: Some(Duration::Permanent),
            target: Some(TargetFilter::LastCreated),
            end_cost: None,
        },
        vec![],
        life,
        P0,
    );
    engine::game::effects::effect::resolve(state, &ability, &mut Vec::new()).unwrap();
    assert_eq!(state.transient_continuous_effects.len(), before);
    assert!(matches_target_filter(
        state,
        token,
        &TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded])),
        &FilterContext::neutral(),
    ));
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
