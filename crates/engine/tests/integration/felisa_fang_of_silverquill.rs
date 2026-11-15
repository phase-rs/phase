//! Felisa, Fang of Silverquill — departure-counter LKI regression.
//!
//! Felisa counts counters on the creature that died, not on a later
//! incarnation of that card. Goryo's Vengeance makes that distinction visible:
//! it returns the card before Felisa resolves, so a live-object/cache lookup
//! would see the counterless returned creature instead of the departure record.

use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityDefinition, Effect, ObjectScope, PtValue, QuantityExpr, QuantityRef,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::zones::Zone;

const FELISA_ORACLE: &str = "Flying\nMentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nWhenever a nontoken creature you control dies, if it had counters on it, create X tapped 2/1 white and black Inkling creature tokens with flying, where X is the number of counters it had on it.";

const GORYOS_VENGEANCE_ORACLE: &str = "Return target legendary creature card from your graveyard to the battlefield. That creature gains haste. Exile it at the beginning of the next end step.\nSplice onto Arcane {2}{B} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";

fn has_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(*definition.effect, Effect::Unimplemented { .. })
        || matches!(
            definition.effect.as_ref(),
            Effect::CreateDelayedTrigger { effect, .. } if has_unimplemented(effect)
        )
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(has_unimplemented)
        || definition
            .else_ability
            .as_deref()
            .is_some_and(has_unimplemented)
        || definition.mode_abilities.iter().any(has_unimplemented)
}

fn object_has_unimplemented(object: &GameObject) -> bool {
    object.abilities.iter().any(has_unimplemented)
        || object.trigger_definitions.as_slice().iter().any(|entry| {
            entry
                .definition
                .execute
                .as_deref()
                .is_some_and(has_unimplemented)
        })
}

fn stack_entries_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn felisa_with_counter_victim() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let felisa = scenario
        .add_creature(P0, "Felisa, Fang of Silverquill", 3, 2)
        .from_oracle_text_with_keywords(&["Flying", "Mentor"], FELISA_ORACLE)
        .id();
    let victim = scenario
        .add_creature(P0, "Counter Victim", 1, 1)
        .with_plus_counters(3)
        .id();
    (scenario.build(), felisa, victim)
}

/// CR 603.4 + CR 603.10a + CR 122.2: Felisa's intervening-if and X both read
/// the dying creature's exact departure snapshot. Reanimating that creature
/// before the trigger resolves must not turn three counters into zero.
#[test]
fn felisa_counts_departure_counters_after_goryos_reanimates_the_victim() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    let felisa = {
        let mut card = scenario.add_creature(P0, "Felisa, Fang of Silverquill", 3, 2);
        card.as_legendary()
            .with_subtypes(vec!["Vampire", "Wizard"])
            .from_oracle_text_with_keywords(&["Flying", "Mentor"], FELISA_ORACLE);
        card.id()
    };
    let victim = scenario
        .add_creature(P0, "Legendary Counter Victim", 1, 1)
        .as_legendary()
        .with_plus_counters(3)
        .id();
    let goryos = {
        let mut card = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Goryo's Vengeance",
            true,
            GORYOS_VENGEANCE_ORACLE,
        );
        card.with_subtypes(vec!["Arcane"])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Black],
            })
            .from_oracle_text_with_keywords(&["Splice"], GORYOS_VENGEANCE_ORACLE);
        card.id()
    };
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    assert!(
        !object_has_unimplemented(&runner.state().objects[&felisa]),
        "Felisa's complete Oracle text must parse without an Unimplemented effect"
    );
    assert!(
        !object_has_unimplemented(&runner.state().objects[&goryos]),
        "Goryo's complete Oracle text, including Splice, must parse without an Unimplemented effect"
    );

    let dies_trigger = runner.state().objects[&felisa]
        .trigger_definitions
        .as_slice()
        .iter()
        .find(|entry| entry.definition.origin == Some(Zone::Battlefield))
        .expect("Felisa must have a battlefield-departure trigger");
    let execute = dies_trigger
        .definition
        .execute
        .as_deref()
        .expect("Felisa dies trigger must have an effect");
    let Effect::Token {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
        ..
    } = execute.effect.as_ref()
    else {
        panic!(
            "Felisa must create a typed token effect, got {:?}",
            execute.effect
        );
    };
    assert_eq!(name, "Inkling");
    assert_eq!(power, &PtValue::Fixed(2));
    assert_eq!(toughness, &PtValue::Fixed(1));
    assert!(types.iter().any(|ty| ty == "Creature"));
    assert!(types.iter().any(|ty| ty == "Inkling"));
    assert_eq!(colors.len(), 2);
    assert!(keywords.contains(&Keyword::Flying));
    assert!(*tapped);
    assert_eq!(
        count,
        &QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::EventSource,
                counter_type: None,
            },
        },
        "Felisa's X must name the creature that died"
    );

    let mut death_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        victim,
        Zone::Graveyard,
        &mut death_events,
    );
    process_triggers(runner.state_mut(), &death_events);
    drain_order_triggers_with_identity(runner.state_mut());
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(
        stack_entries_from(&runner, felisa),
        1,
        "Felisa must be pending before the reanimation response"
    );

    {
        let committed = runner.cast(goryos).target_object(victim).commit();
        assert_eq!(
            committed.state().stack.len(),
            2,
            "Goryo's must be committed above Felisa before either resolves"
        );
    }
    runner.resolve_top();

    let returned = &runner.state().objects[&victim];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(
        returned.counters.is_empty(),
        "the re-entered creature is a new, counterless incarnation"
    );
    assert_eq!(
        stack_entries_from(&runner, felisa),
        1,
        "resolving Goryo's must leave Felisa pending"
    );

    runner.resolve_top();

    let inklings: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.controller == P0
                && object.owner == P0
                && object.name == "Inkling"
        })
        .collect();
    assert_eq!(
        inklings.len(),
        3,
        "Felisa creates one token per departed counter"
    );
    for inkling in inklings {
        assert!(inkling.tapped);
        assert_eq!(inkling.power, Some(2));
        assert_eq!(inkling.toughness, Some(1));
        assert!(inkling.card_types.core_types.contains(&CoreType::Creature));
        assert!(inkling.card_types.subtypes.iter().any(|ty| ty == "Inkling"));
        assert!(inkling
            .color
            .contains(&engine::types::mana::ManaColor::White));
        assert!(inkling
            .color
            .contains(&engine::types::mana::ManaColor::Black));
        assert!(inkling.keywords.contains(&Keyword::Flying));
    }

    assert_eq!(
        runner.state().objects[&victim]
            .counters
            .get(&CounterType::Plus1Plus1),
        None,
        "the token count came from the departure record, not the returned object"
    );
}

/// A present but incoherent departure context is not a legacy record. It must
/// fail closed at trigger detection instead of consulting the mutable LKI cache.
#[test]
fn felisa_does_not_trigger_from_a_malformed_departure_record() {
    let (mut unmodified_runner, unmodified_felisa, unmodified_victim) =
        felisa_with_counter_victim();
    let mut unmodified_death_events = Vec::new();
    move_to_zone(
        unmodified_runner.state_mut(),
        unmodified_victim,
        Zone::Graveyard,
        &mut unmodified_death_events,
    );
    process_triggers(unmodified_runner.state_mut(), &unmodified_death_events);
    drain_order_triggers_with_identity(unmodified_runner.state_mut());
    assert_eq!(
        stack_entries_from(&unmodified_runner, unmodified_felisa),
        1,
        "the unmodified production death event reach-guards Felisa's trigger path"
    );

    let (mut runner, felisa, victim) = felisa_with_counter_victim();

    let mut death_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        victim,
        Zone::Graveyard,
        &mut death_events,
    );
    let record = death_events
        .iter_mut()
        .find_map(|event| match event {
            engine::types::events::GameEvent::ZoneChanged {
                object_id, record, ..
            } if *object_id == victim => Some(record),
            _ => None,
        })
        .expect("production death must emit a ZoneChanged record");
    record
        .trigger_source_context
        .as_mut()
        .expect("production record has owned context")
        .identity
        .expected_zone = Zone::Graveyard;

    process_triggers(runner.state_mut(), &death_events);
    drain_order_triggers_with_identity(runner.state_mut());
    assert_eq!(
        stack_entries_from(&runner, felisa),
        0,
        "a malformed context must not be silently downgraded to cache fallback"
    );
}
