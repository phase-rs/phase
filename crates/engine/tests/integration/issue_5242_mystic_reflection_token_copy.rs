//! Issue #5242: Mystic Reflection's delayed replacement must make the next
//! creature/planeswalker entry copy the creature chosen when Mystic resolved.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.";

#[test]
fn mystic_reflection_makes_next_creature_token_copy_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create a 1/1 white Soldier creature token.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(token_spell).resolve();

    let copied_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("token spell must create one token");
    let obj = runner
        .state()
        .objects
        .get(&copied_token)
        .expect("created token must exist");

    assert!(obj.is_token, "the entering object must still be a token");
    assert_eq!(obj.power, Some(6), "token must copy chosen creature power");
    assert_eq!(
        obj.toughness,
        Some(6),
        "token must copy chosen creature toughness"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "token must copy chosen creature subtype, got {:?}",
        obj.card_types.subtypes
    );
}

#[test]
fn mystic_reflection_copies_every_token_in_one_multi_token_entry_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(token_spell).resolve();

    // CR 614.12: One replacement effect modifies every permanent entering in
    // this simultaneous two-token event before the one-shot shield is consumed.
    assert_eq!(
        runner.state().last_created_token_ids.len(),
        2,
        "the production token pipeline must create both tokens in one event"
    );
    for token_id in &runner.state().last_created_token_ids {
        let obj = runner
            .state()
            .objects
            .get(token_id)
            .expect("each created token must exist");
        assert!(obj.is_token, "the entering object must remain a token");
        assert_eq!(obj.name, "Colossal Dreadmaw");
        assert_eq!(obj.power, Some(6));
        assert_eq!(obj.toughness, Some(6));
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
            "every token in the entry event must copy the chosen subtype, got {:?}",
            obj.card_types.subtypes
        );
    }
}

#[test]
fn mystic_reflection_makes_next_nontoken_creature_enter_as_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let entering = scenario
        .add_creature_to_hand(P0, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let follow_up = scenario
        .add_creature_to_hand(P0, "Silvercoat Lion", 2, 2)
        .with_subtypes(vec!["Cat"])
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(entering).resolve();

    let obj = runner
        .state()
        .objects
        .get(&entering)
        .expect("creature card must enter the battlefield");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(!obj.is_token, "copied entering card must remain non-token");
    assert_eq!(obj.name, "Colossal Dreadmaw");
    assert_eq!(obj.power, Some(6));
    assert_eq!(obj.toughness, Some(6));
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "copied entering card must be a creature, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "copied entering card must copy subtype, got {:?}",
        obj.card_types.subtypes
    );

    runner.cast(follow_up).resolve();
    let second = runner
        .state()
        .objects
        .get(&follow_up)
        .expect("follow-up creature must enter the battlefield");
    assert_eq!(second.name, "Silvercoat Lion");
    assert_eq!(second.power, Some(2));
    assert_eq!(second.toughness, Some(2));
    assert!(
        second.card_types.subtypes.iter().any(|s| s == "Cat"),
        "one-shot replacement must be consumed before the next entry"
    );
}

#[test]
fn mystic_reflection_makes_next_planeswalker_enter_as_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let entering = scenario
        .add_creature_to_hand(P0, "Jace Beleren", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 3)
        .id();
    let follow_up = scenario
        .add_creature_to_hand(P0, "Mesa Lynx", 2, 1)
        .with_subtypes(vec!["Cat"])
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(entering).resolve();

    let obj = runner
        .state()
        .objects
        .get(&entering)
        .expect("planeswalker card must enter the battlefield");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(!obj.is_token, "copied entering card must remain non-token");
    assert_eq!(obj.name, "Colossal Dreadmaw");
    assert_eq!(obj.power, Some(6));
    assert_eq!(obj.toughness, Some(6));
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "planeswalker entry must copy creature card type, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Planeswalker),
        "planeswalker entry must not retain its original card type"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "planeswalker entry must copy subtype, got {:?}",
        obj.card_types.subtypes
    );
    assert_eq!(
        obj.loyalty, None,
        "copied creature characteristics have no loyalty"
    );
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty).copied(),
        None,
        "intrinsic planeswalker counters must be retargeted to copied characteristics"
    );

    runner.cast(follow_up).resolve();
    let second = runner
        .state()
        .objects
        .get(&follow_up)
        .expect("follow-up creature must enter the battlefield");
    assert_eq!(second.name, "Mesa Lynx");
    assert_eq!(second.power, Some(2));
    assert_eq!(second.toughness, Some(1));
    assert!(
        second.card_types.subtypes.iter().any(|s| s == "Cat"),
        "one-shot replacement must be consumed before the next entry"
    );
}

#[test]
fn mystic_reflection_uses_lki_when_chosen_token_ceases_to_exist() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source_token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bestial Menace",
            true,
            "Create a 3/3 green Elephant creature token.",
        )
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create a 1/1 white Soldier creature token.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(source_token_spell).resolve();
    let chosen_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("source token spell must create a token");

    runner.cast(mystic).target_object(chosen_token).resolve();

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        chosen_token,
        Zone::Graveyard,
        &mut events,
    );
    check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        !runner.state().objects.contains_key(&chosen_token),
        "the chosen token must cease to exist before the replacement applies"
    );

    runner.cast(token_spell).resolve();

    let copied_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("second token spell must create one token");
    let obj = runner
        .state()
        .objects
        .get(&copied_token)
        .expect("created token must exist");

    assert_eq!(obj.power, Some(3), "token must copy LKI power");
    assert_eq!(obj.toughness, Some(3), "token must copy LKI toughness");
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Elephant"),
        "token must copy LKI subtype, got {:?}",
        obj.card_types.subtypes
    );
}
