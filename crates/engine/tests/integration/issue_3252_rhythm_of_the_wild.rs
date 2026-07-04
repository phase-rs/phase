//! Issue #3252 — Rhythm of the Wild riot applies when a nontoken creature
//! enters the battlefield without being cast (Atla Palani egg trigger).
//!
//! https://github.com/phase-rs/phase/issues/3252

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, Effect, FilterProp, ResolvedAbility, TargetFilter, TargetRef,
    TypeFilter, TypedFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::{EtbTapState, Zone};

const RHYTHM_OF_THE_WILD: &str = "Creature spells you control can't be countered.\n\
Nontoken creatures you control have riot. (They enter with your choice of a +1/+1 counter or haste.)";

const ATLA_ORACLE: &str = "{2}, {T}: Create a 0/1 green Egg creature token with defender.\n\
Whenever an Egg you control dies, reveal cards from the top of your library until you reveal a creature card. Put that card onto the battlefield and the rest on the bottom of your library in a random order.";

fn put_library_top(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let zone = runner.state().objects.get(&id).expect("object").zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner
            .state_mut()
            .players
            .get_mut(owner.0 as usize)
            .unwrap()
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

fn assert_riot_replacement_choice(runner: &engine::game::scenario::GameRunner) {
    let WaitingFor::ReplacementChoice {
        candidate_count,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "granted Riot should prompt as an ETB replacement; waiting_for={:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*candidate_count, 2);
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.description.contains("Riot")),
        "replacement choice should identify Riot, got {candidates:?}"
    );
}

#[test]
fn rhythm_of_the_wild_grants_riot_on_library_to_battlefield_put() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rhythm = scenario
        .add_creature_from_oracle(P0, "Rhythm of the Wild", 0, 0, RHYTHM_OF_THE_WILD)
        .as_enchantment()
        .id();

    let wurm = scenario.add_creature(P0, "Worldspine Wurm", 15, 15).id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, wurm);

    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::SpecificObject { id: wurm },
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(wurm)],
        rhythm,
        P0,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    assert_riot_replacement_choice(&runner);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose Riot counter");
    assert_eq!(
        runner.state().objects[&wurm].zone,
        Zone::Battlefield,
        "Worldspine Wurm should be on the battlefield"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&wurm)
            .and_then(|o| o
                .counters
                .get(&engine::types::counter::CounterType::Plus1Plus1))
            .copied(),
        Some(1),
        "accepting Riot should make the creature enter with a +1/+1 counter"
    );
}

#[test]
fn atla_egg_trigger_puts_creature_with_rhythm_of_the_wild_riot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Rhythm of the Wild", 0, 0, RHYTHM_OF_THE_WILD)
        .as_enchantment();

    let atla = scenario
        .add_creature_from_oracle(P0, "Atla Palani, Nest Tender", 2, 3, ATLA_ORACLE)
        .id();

    let wurm = scenario.add_creature(P0, "Worldspine Wurm", 15, 15).id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    put_library_top(&mut runner, wurm);

    runner.activate(atla, 0).resolve();

    let egg = runner
        .state()
        .objects
        .iter()
        .find(|(_, obj)| {
            obj.zone == Zone::Battlefield && obj.card_types.subtypes.iter().any(|s| s == "Egg")
        })
        .map(|(id, _)| *id)
        .expect("Atla activation should create an Egg token");

    runner
        .act(GameAction::Debug(DebugAction::Sacrifice { object_id: egg }))
        .expect("sacrifice egg to trigger Atla");

    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    assert_riot_replacement_choice(&runner);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose Riot counter");
    assert_eq!(
        runner.state().objects[&wurm].zone,
        Zone::Battlefield,
        "Worldspine Wurm should be on the battlefield after the egg trigger"
    );
}

#[test]
fn rhythm_of_the_wild_oracle_parses_cant_be_countered_and_riot_grant() {
    let parsed = parse_oracle_text(
        RHYTHM_OF_THE_WILD,
        "Rhythm of the Wild",
        &[],
        &["Enchantment".to_string()],
        &[],
    );

    let cant_counter = parsed
        .statics
        .iter()
        .find(|s| s.mode == StaticMode::CantBeCountered)
        .expect("Rhythm must parse cant-be-countered static");
    match cant_counter.affected.as_ref() {
        Some(TargetFilter::Typed(TypedFilter { type_filters, .. })) => {
            assert!(
                type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Creature)),
                "cant-be-countered subject must be creature spells"
            );
        }
        other => panic!("expected typed creature-spell filter, got {other:?}"),
    }

    let riot_grant = parsed
        .statics
        .iter()
        .find(|s| {
            s.mode == StaticMode::Continuous
                && s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Riot
                        }
                    )
                })
        })
        .expect("Rhythm must parse riot continuous grant");
    match riot_grant.affected.as_ref() {
        Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
            assert!(
                properties.contains(&FilterProp::NonToken),
                "riot grant must be limited to nontoken creatures"
            );
        }
        other => panic!("expected typed nontoken creature filter, got {other:?}"),
    }
}

#[test]
fn cast_rhythm_of_the_wild_then_atla_egg_trigger_still_gets_riot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rhythm = scenario
        .add_creature_to_hand_from_oracle(P0, "Rhythm of the Wild", 0, 0, RHYTHM_OF_THE_WILD)
        .as_enchantment()
        .id();

    let atla = scenario
        .add_creature_from_oracle(P0, "Atla Palani, Nest Tender", 2, 3, ATLA_ORACLE)
        .id();

    let wurm = scenario.add_creature(P0, "Worldspine Wurm", 15, 15).id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    put_library_top(&mut runner, wurm);

    runner.cast(rhythm).resolve();
    runner.activate(atla, 0).resolve();

    let egg = runner
        .state()
        .objects
        .iter()
        .find(|(_, obj)| {
            obj.zone == Zone::Battlefield && obj.card_types.subtypes.iter().any(|s| s == "Egg")
        })
        .map(|(id, _)| *id)
        .expect("Atla activation should create an Egg token");

    runner
        .act(GameAction::Debug(DebugAction::Sacrifice { object_id: egg }))
        .expect("sacrifice egg to trigger Atla");

    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    assert_riot_replacement_choice(&runner);
}
