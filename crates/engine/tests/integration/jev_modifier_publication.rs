//! Continuous instructions retain their complete subject, axes and duration.

use super::jev_defining_destroy::{cost, fund, settled};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn printed(name: &str) -> serde_json::Value {
    // Pinned INPUT only; expected outcomes below are independently specified.
    let fixture: serde_json::Value = serde_json::from_reader(flate2::read::GzDecoder::new(
        &include_bytes!("../fixtures/jev_defining_modifier_atomic_cards.json.gz")[..],
    ))
    .unwrap();
    assert_eq!(
        fixture["atomic_cards_sha256"],
        "EE8B89375A0328A534153B1121E4272050AEF69CD49C58387AEF0877D34E4F8E"
    );
    fixture["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|face| face["faceName"].as_str().or_else(|| face["name"].as_str()) == Some(name))
        .unwrap_or_else(|| panic!("missing complete pinned input {name}"))
        .clone()
}

fn pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    engine::game::layers::evaluate_layers(runner.state_mut());
    let object = &runner.state().objects[&id];
    (object.power.unwrap(), object.toughness.unwrap())
}

#[test]
fn subtype_transformations_keep_other_core_types_and_restore_at_real_cleanup() {
    // CR 205.1a / CR 611.2a: subtype replacement preserves other core types,
    // and the printed duration ends at cleanup rather than source departure.
    for (name, oracle, mana, generic, colors, subtype, color, base, draw) in [
        ("Turn to Frog", "Until end of turn, target creature loses all abilities and becomes a blue Frog with base power and toughness 1/1.", "{1}{U}", 1, vec![ManaType::Blue], "Frog", ManaColor::Blue, (1, 1), false),
        ("Amphibian Accident", "Until end of turn, target creature loses all abilities and becomes a blue Frog with base power and toughness 1/1.", "{1}{U}", 1, vec![ManaType::Blue], "Frog", ManaColor::Blue, (1, 1), false),
        ("Gift of Tusks", "Until end of turn, target creature loses all abilities and becomes a green Elephant with base power and toughness 3/3.", "{U}", 0, vec![ManaType::Blue], "Elephant", ManaColor::Green, (3, 3), false),
        ("Snakeform", "Until end of turn, target creature loses all abilities and becomes a green Snake with base power and toughness 1/1.\nDraw a card.", "{2}{G/U}", 2, vec![ManaType::Blue], "Snake", ManaColor::Green, (1, 1), true),
        ("Turn", "Until end of turn, target creature loses all abilities and becomes a red Weird with base power and toughness 0/1.\nFuse (You may cast one or both halves of this card from your hand.)", "{2}{U}", 2, vec![ManaType::Blue], "Weird", ManaColor::Red, (0, 1), false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P1, "Legendary artifact Goblin", 5, 6).as_artifact_creature().as_legendary().with_subtypes(vec!["Goblin"]).with_color(vec![ManaColor::White]).with_plus_counters(2).flying().id();
        let decoy = scenario.add_creature(P1, "Unselected", 7, 8).flying().id();
        let drawn = scenario.add_card_to_library_top(P0, "Snakeform independent Draw");
        let spell = scenario.add_spell_to_hand_from_oracle(P0, name, true, oracle).with_mana_cost(cost(mana)).id();
        let later = scenario.add_spell_to_hand_from_oracle(P0, "Leap", true, "Target creature gains flying until end of turn.\nDraw a card.").with_mana_cost(cost("{U}")).id();
        scenario.add_card_to_library_top(P0, "Leap draw");
        let mut runner = scenario.build();
        fund(&mut runner, generic, &colors);
        let outcome = runner.cast(spell).target_object(target).resolve();
        outcome.assert_hand_drawn(P0, i64::from(draw));
        assert_eq!(pt(&mut runner, target), (base.0 + 2, base.1 + 2));
        let object = &runner.state().objects[&target];
        assert!(object.card_types.core_types.contains(&CoreType::Artifact));
        assert!(object.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(object.card_types.subtypes, vec![subtype.to_owned()]);
        assert_eq!(object.color, vec![color]);
        assert!(!object.keywords.contains(&Keyword::Flying));
        assert!(runner.state().objects[&decoy].keywords.contains(&Keyword::Flying));
        settled(&runner);
        fund(&mut runner, 0, &[ManaType::Blue]);
        runner.cast(later).target_object(target).resolve();
        assert!(runner.state().objects[&target].keywords.contains(&Keyword::Flying), "later grant survives earlier removal");
        runner.advance_to_upkeep();
        assert_eq!(pt(&mut runner, target), (7, 8));
        assert_eq!(runner.state().objects[&target].card_types.subtypes, vec!["Goblin".to_owned()]);
        assert_eq!(runner.state().objects[&target].color, vec![ManaColor::White]);
        assert!(runner.state().objects[&target].keywords.contains(&Keyword::Flying));
        assert!(matches!(runner.state().objects[&drawn].zone, Zone::Hand | Zone::Library));
    }
}

#[test]
fn jest_uses_the_target_players_frozen_creature_cohort() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let affected = scenario
        .add_creature(P1, "Artifact Elf", 4, 4)
        .as_artifact_creature()
        .with_subtypes(vec!["Elf"])
        .flying()
        .id();
    let own = scenario
        .add_creature(P0, "Own Elf", 4, 4)
        .with_subtypes(vec!["Elf"])
        .flying()
        .id();
    let late = scenario
        .add_creature_to_hand(P1, "Later arrival", 3, 3)
        .flying()
        .id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Polymorphist's Jest", true,
        "Until end of turn, each creature target player controls loses all abilities and becomes a blue Frog with base power and toughness 1/1.").with_mana_cost(cost("{1}{U}{U}")).id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::Blue; 2]);
    runner.cast(spell).target_player(P1).resolve();
    assert_eq!(pt(&mut runner, affected), (1, 1));
    assert!(runner.state().objects[&affected]
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert_eq!(pt(&mut runner, own), (4, 4));
    engine::game::zones::move_to_zone(runner.state_mut(), late, Zone::Battlefield, &mut vec![]);
    assert_eq!(pt(&mut runner, late), (3, 3));
    assert!(runner.state().objects[&late]
        .keywords
        .contains(&Keyword::Flying));
    settled(&runner);
}

#[test]
fn oko_explicit_creature_replaces_artifact_but_lasts_after_source_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_planeswalker_from_oracle(P0, "Oko, Thief of Crowns", "Oko", 4,
        "[+2]: Create a Food token. (It's an artifact with \"{2}, {T}, Sacrifice this token: You gain 3 life.\")\n[+1]: Target artifact or creature loses all abilities and becomes a green Elk creature with base power and toughness 3/3.\n[−5]: Exchange control of target artifact or creature you control and target creature an opponent controls with power 3 or less.").id();
    let target = scenario
        .add_artifact_from_oracle(P1, "Legendary artifact", "{T}: Add {C}.")
        .as_legendary()
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    runner.activate(source, 1).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (3, 3));
    assert_eq!(
        runner.state().objects[&target].card_types.core_types,
        vec![CoreType::Creature]
    );
    assert_eq!(
        runner.state().objects[&target].card_types.subtypes,
        vec!["Elk".to_owned()]
    );
    assert!(runner.state().objects[&target].abilities.is_empty());
    fund(&mut runner, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    runner.advance_to_upkeep();
    assert_eq!(pt(&mut runner, target), (3, 3));
    assert_eq!(
        runner.state().objects[&target].card_types.core_types,
        vec![CoreType::Creature]
    );
}

#[test]
fn disciple_gives_the_declared_foreign_creature_both_flying_and_blue() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Disciple of Kangee",
            2,
            2,
            "{U}, {T}: Target creature gains flying and becomes blue until end of turn.",
        )
        .with_color(vec![ManaColor::White])
        .id();
    let target = scenario
        .add_creature(P1, "Foreign red target", 3, 3)
        .with_color(vec![ManaColor::Red])
        .id();
    let decoy = scenario
        .add_creature(P1, "Other red", 4, 4)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 0, &[ManaType::Blue]);
    runner.activate(source, 0).target_object(target).resolve();
    assert_eq!(runner.state().objects[&target].color, vec![ManaColor::Blue]);
    assert!(runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Flying));
    assert_eq!(runner.state().objects[&decoy].color, vec![ManaColor::Red]);
    assert!(!runner.state().objects[&source]
        .keywords
        .contains(&Keyword::Flying));
    assert!(runner.state().objects[&source].tapped);
    settled(&runner);
    runner.advance_to_upkeep();
    assert_eq!(runner.state().objects[&target].color, vec![ManaColor::Red]);
    assert!(!runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Flying));
}

#[test]
fn compound_zealous_persecution_preserves_both_subjects_even_when_one_is_empty() {
    // CR 611.2c: each resolving population is frozen independently.
    for empty_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let own = (!empty_first).then(|| scenario.add_creature(P0, "Own creature", 3, 4).id());
        let opposing = scenario.add_creature(P1, "Opposing creature", 4, 5).id();
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Zealous Persecution", true,
            "Until end of turn, creatures you control get +1/+1 and creatures your opponents control get -1/-1.").with_mana_cost(cost("{W}{B}")).id();
        let mut runner = scenario.build();
        fund(&mut runner, 0, &[ManaType::White, ManaType::Black]);
        runner.cast(spell).resolve();
        if let Some(own) = own {
            assert_eq!(pt(&mut runner, own), (4, 5));
        }
        assert_eq!(pt(&mut runner, opposing), (3, 4));
        settled(&runner);
        runner.advance_to_upkeep();
        assert_eq!(pt(&mut runner, opposing), (4, 5));
    }
}

#[test]
fn agrus_red_white_and_dual_attackers_keep_independent_pumps() {
    use engine::game::combat::AttackTarget;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Agrus Kos, Wojek Veteran", 3, 3,
        "Whenever Agrus Kos attacks, attacking red creatures get +2/+0 and attacking white creatures get +0/+2 until end of turn.").with_color(vec![ManaColor::Red, ManaColor::White]).id();
    let red = scenario
        .add_creature(P0, "Red attacker", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    let white = scenario
        .add_creature(P0, "White attacker", 2, 2)
        .with_color(vec![ManaColor::White])
        .id();
    let blue = scenario
        .add_creature(P0, "Blue attacker", 2, 2)
        .with_color(vec![ManaColor::Blue])
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner.declare_attackers(vec![
        (source, AttackTarget::Player(P1)),
        (red, AttackTarget::Player(P1)),
        (white, AttackTarget::Player(P1)),
        (blue, AttackTarget::Player(P1)),
    ]);
    runner.advance_until_stack_empty();
    assert_eq!(pt(&mut runner, source), (5, 5));
    assert_eq!(pt(&mut runner, red), (4, 2));
    assert_eq!(pt(&mut runner, white), (2, 4));
    assert_eq!(pt(&mut runner, blue), (2, 2));
    settled(&runner);
}

#[test]
fn arcane_lighthouse_removes_and_denies_both_abilities_on_its_frozen_cohort() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_land_from_oracle(P0, "Arcane Lighthouse", "{T}: Add {C}.\n{1}, {T}: Until end of turn, creatures your opponents control lose hexproof and shroud and can't have hexproof or shroud.").id();
    let target = scenario
        .add_creature(P1, "Affected", 3, 3)
        .hexproof()
        .with_keyword(Keyword::Shroud)
        .id();
    let own = scenario
        .add_creature(P0, "Own", 3, 3)
        .hexproof()
        .with_keyword(Keyword::Shroud)
        .id();
    let late = scenario
        .add_creature_to_hand(P1, "Late", 3, 3)
        .hexproof()
        .with_keyword(Keyword::Shroud)
        .id();
    // Secondary typed class composition, paired with the complete Lighthouse.
    let regrant = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Secondary hexproof composition",
            true,
            "Creatures your opponents control gain hexproof until end of turn.",
        )
        .with_mana_cost(cost("{1}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[]);
    runner.activate(source, 1).resolve();
    for keyword in [Keyword::Hexproof, Keyword::Shroud] {
        assert!(!runner.state().objects[&target].keywords.contains(&keyword));
        assert!(runner.state().objects[&own].keywords.contains(&keyword));
    }
    engine::game::zones::move_to_zone(runner.state_mut(), late, Zone::Battlefield, &mut vec![]);
    fund(&mut runner, 1, &[]);
    runner.cast(regrant).resolve();
    assert!(!runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Hexproof));
    assert!(runner.state().objects[&late]
        .keywords
        .contains(&Keyword::Hexproof));
    settled(&runner);
    runner.advance_to_upkeep();
    for keyword in [Keyword::Hexproof, Keyword::Shroud] {
        assert!(runner.state().objects[&target].keywords.contains(&keyword));
    }
}

#[test]
fn explore_real_etb_chooses_graveyard_and_sequential_explores_retire_each_choice() {
    // CR 701.44d: each explore is a distinct instruction, with its own choice.
    for (name, oracle, mana, generic, green, count) in [
        ("Cenote Scout", "When this creature enters, it explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on this creature, then put the card back or put it into your graveyard.)", "{G}", 0, 1, 1),
        ("Jadelight Ranger", "When this creature enters, it explores, then it explores again. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on this creature, then put the card back or put it into your graveyard. Then repeat this process.)", "{1}{G}{G}", 1, 2, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let second = scenario.add_spell_to_library_top(P0, "Second nonland", true).id();
        let first = scenario.add_spell_to_library_top(P0, "First nonland", true).id();
        let source = scenario.add_creature_to_hand_from_oracle(P0, name, 1, 1, oracle).with_mana_cost(cost(mana)).id();
        let mut runner = scenario.build();
        fund(&mut runner, generic, &vec![ManaType::Green; green]);
        runner.cast(source).resolve();
        for expected in [first, second].into_iter().take(count) {
            let WaitingFor::DigChoice { player, cards, .. } = &runner.state().waiting_for else { panic!("actual revealed nonland explore choice: {:?}", runner.state().waiting_for); };
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![expected]);
            runner.act(GameAction::SelectCards { cards: vec![expected] }).unwrap();
        }
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
        if count == 2 { assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard); }
        assert_eq!(runner.state().objects[&source].counters.get(&CounterType::Plus1Plus1).copied().unwrap_or(0), count as u32);
        settled(&runner);
    }
}

#[test]
fn hakbal_actual_nondefault_explorer_order_keeps_each_recipient_once() {
    use engine::types::ability::TargetRef;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Hakbal of the Surging Soul", 3, 3,
        "At the beginning of combat on your turn, each Merfolk creature you control explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the exploring creature, then put the card back or put it into your graveyard.)\nWhenever Hakbal attacks, you may put a land card from your hand onto the battlefield. If you don't, draw a card.").with_subtypes(vec!["Merfolk", "Scout"]).id();
    let first = scenario
        .add_creature(P0, "First sibling", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let second = scenario
        .add_creature(P0, "Second sibling", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let other = scenario
        .add_creature(P1, "Other seat Merfolk", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    for i in 0..3 {
        scenario.add_land_to_library_top(P0, &format!("Explore land {i}"));
    }
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let WaitingFor::ExploreChoice {
        player, choosable, ..
    } = &runner.state().waiting_for
    else {
        panic!("natural beginning combat exploration order");
    };
    assert_eq!(*player, P0);
    assert_eq!(choosable.len(), 3);
    assert!(!choosable.contains(&other));
    assert_ne!(
        choosable[0], second,
        "explicit nondefault order discriminator"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second)),
        })
        .unwrap();
    let WaitingFor::ExploreChoice { choosable, .. } = &runner.state().waiting_for else {
        panic!("second order prompt");
    };
    assert!(!choosable.contains(&second));
    assert!(choosable.contains(&first) && choosable.contains(&source));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(source)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].hand.len(), 3);
    assert_eq!(runner.state().players[1].hand.len(), 0);
    settled(&runner);
}

#[test]
fn weeping_angels_actual_opponent_cast_removes_creature_and_correlated_subtypes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_to_hand_from_oracle(P0, "Weeping Angel", 2, 2,
        "Flash\nFirst strike, vigilance\nWhenever an opponent casts a creature spell, this creature isn't a creature until end of turn.\nIf this creature would deal combat damage to a creature, prevent that damage and that creature's owner shuffles it into their library.").as_artifact_creature().with_subtypes(vec!["Alien", "Angel"]).with_mana_cost(cost("{1}{U}{B}")).id();
    let foreign = scenario
        .add_creature_to_hand(P1, "Actual opponent creature spell", 1, 1)
        .with_mana_cost(cost("{1}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::Blue, ManaType::Black]);
    runner.cast(source).resolve();
    assert!(runner.state().objects[&source]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    runner.act(GameAction::PassPriority).unwrap();
    runner.state_mut().players[1]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    runner.cast(foreign).resolve();
    assert_eq!(
        runner.state().objects[&source].card_types.core_types,
        vec![CoreType::Artifact]
    );
    assert!(runner.state().objects[&source]
        .card_types
        .subtypes
        .is_empty());
    assert_eq!(runner.state().objects[&foreign].zone, Zone::Battlefield);
    settled(&runner);
    runner.advance_to_upkeep();
    assert!(runner.state().objects[&source]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(
        runner.state().objects[&source].card_types.subtypes,
        vec!["Alien".to_owned(), "Angel".to_owned()]
    );
}

#[test]
fn spark_rupture_draw_and_live_recipient_loyalty_do_not_use_source_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_planeswalker_from_oracle(P0, "Two loyalty", "Jace", 2, "[+1]: Draw a card.")
        .id();
    let second = scenario
        .add_planeswalker_from_oracle(P1, "Five loyalty", "Chandra", 5, "[+1]: Draw a card.")
        .id();
    let remover = scenario
        .add_creature_from_oracle(
            P0,
            "Vampire Hexmage",
            2,
            1,
            "First strike\nSacrifice this creature: Remove all counters from target permanent.",
        )
        .id();
    // Explicit secondary planeswalker composition for the later-arrival seam.
    let later = scenario
        .add_spell_to_hand(P0, "Later planeswalker", false)
        .as_planeswalker_with_loyalty("Jace", 3)
        .with_mana_cost(cost("{1}"))
        .from_oracle_text("[+1]: Draw a card.")
        .id();
    let drawn = scenario.add_card_to_library_top(P0, "Independent ETB Draw");
    let source = scenario.add_spell_to_hand_from_oracle(P0, "Spark Rupture", false,
        "When this enchantment enters, draw a card.\nEach planeswalker with one or more loyalty counters on it loses all abilities and is a creature with power and toughness each equal to the number of loyalty counters on it.").as_enchantment().with_mana_cost(cost("{2}{W}")).id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::White]);
    runner.cast(source).resolve();
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert_eq!(pt(&mut runner, first), (2, 2));
    assert_eq!(pt(&mut runner, second), (5, 5));
    for id in [first, second] {
        assert_eq!(
            runner.state().objects[&id].card_types.core_types,
            vec![CoreType::Creature]
        );
        assert!(runner.state().objects[&id].abilities.is_empty());
    }
    runner
        .state_mut()
        .objects
        .get_mut(&first)
        .unwrap()
        .counters
        .insert(CounterType::Loyalty, 4);
    assert_eq!(pt(&mut runner, first), (4, 4));
    fund(&mut runner, 1, &[]);
    runner.cast(later).resolve();
    assert_eq!(
        pt(&mut runner, later),
        (3, 3),
        "later arrivals join the live static population"
    );
    runner.activate(remover, 0).target_object(second).resolve();
    assert_eq!(runner.state().objects[&remover].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Graveyard,
        "zero loyalty stops the static and exposes the planeswalker state-based action"
    );
    fund(&mut runner, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(source).resolve();
    assert!(runner.state().objects[&first]
        .card_types
        .core_types
        .contains(&CoreType::Planeswalker));
    assert!(!runner.state().objects[&first].abilities.is_empty());
    settled(&runner);
}

#[test]
fn irencrag_natural_entry_accept_installs_exactly_one_real_paid_equip() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_artifact_from_oracle(P0, "The Irencrag",
        "{T}: Add {C}.\nWhenever a legendary creature you control enters, you may have The Irencrag become a legendary Equipment artifact named Everflame, Heroes' Legacy. If you do, it gains equip {3} and \"Equipped creature gets +3/+3\" and loses all other abilities.").as_legendary().id();
        let entering = scenario
            .add_creature_to_hand(P0, "Legendary entry", 2, 2)
            .as_legendary()
            .with_mana_cost(cost("{1}"))
            .id();
        let other = scenario.add_creature(P0, "Other equip target", 3, 3).id();
        let later = scenario
            .add_creature_to_hand(P0, "Later legendary entry", 1, 2)
            .as_legendary()
            .with_mana_cost(cost("{1}"))
            .id();
        let mut runner = scenario.build();
        fund(&mut runner, 1, &[]);
        runner.cast(entering).commit().stop_at_optional().resolve();
        let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for else {
            panic!("natural Irencrag may choice");
        };
        assert_eq!(*player, P0);
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        runner.advance_until_stack_empty();
        if !accept {
            assert_eq!(runner.state().objects[&source].name, "The Irencrag");
            assert!(runner.state().objects[&source].attached_to.is_none());
            settled(&runner);
            continue;
        }
        let definitions =
            engine::game::casting::activated_ability_definitions(runner.state(), source);
        assert_eq!(
            definitions.len(),
            1,
            "old tap mana removed; exactly one equip remains"
        );
        let index = definitions[0].0;
        fund(&mut runner, 3, &[]);
        runner
            .activate(source, index)
            .target_object(other)
            .resolve();
        assert_eq!(pt(&mut runner, other), (6, 6));
        assert_eq!(pt(&mut runner, entering), (2, 2));
        assert_eq!(
            runner.state().objects[&source].attached_to,
            Some(engine::game::game_object::AttachTarget::Object(other))
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        settled(&runner);
        fund(&mut runner, 1, &[]);
        runner.cast(later).commit().stop_at_optional().resolve();
        settled(&runner);
        assert_eq!(
            engine::game::casting::activated_ability_definitions(runner.state(), source).len(),
            1,
            "removed entry trigger does not reoffer or duplicate equip"
        );
        runner.advance_to_upkeep();
        assert_eq!(pt(&mut runner, other), (6, 6));
    }
}

#[test]
fn ferocity_three_actual_upkeeps_damage_and_destroy_the_enchanted_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Enchanted recipient", 2, 9).id();
    let decoy = scenario.add_creature(P1, "Unenchanted decoy", 2, 9).id();
    scenario.with_library_top(P0, &["turn one", "turn two", "turn three", "turn four"]);
    scenario.with_library_top(P1, &["turn one", "turn two", "turn three", "turn four"]);
    let aura = scenario.add_spell_to_hand_from_oracle(P0, "Consuming Ferocity", false,
        "Enchant non-Wall creature\nEnchanted creature gets +1/+0.\nAt the beginning of your upkeep, put a +1/+0 counter on enchanted creature. If that creature has three or more +1/+0 counters on it, it deals damage equal to its power to its controller, then destroy that creature and it can't be regenerated.").as_enchantment().with_subtypes(vec!["Aura"]).with_mana_cost(cost("{1}{R}")).id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::Red]);
    runner.cast(aura).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (3, 9));
    for count in 1..=3 {
        for _ in 0..4 {
            runner.advance_to_phase(Phase::PreCombatMain);
            runner.advance_to_end_step();
            runner.advance_to_upkeep();
            if runner.state().active_player == P0 {
                break;
            }
        }
        assert_eq!(
            runner.state().active_player,
            P0,
            "reached the Aura controller's real upkeep"
        );
        runner.advance_until_stack_empty();
        if count < 3 {
            assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
            assert_eq!(pt(&mut runner, target), (3 + count, 9));
            assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
        }
    }
    assert_eq!(runner.life(P1), 14);
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
    settled(&runner);
}

#[test]
fn really_epic_punch_nonhost_still_fights_without_a_source_self_pump() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Ordinary nonhost", 3, 6).id();
    let foreign = scenario.add_creature(P1, "Fight opponent", 2, 7).id();
    let decoy = scenario.add_creature(P0, "Unselected own", 8, 8).id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Really Epic Punch", false,
        "Target creature you control gets +2/+2 if it's a host or has augment. Then it fights target creature you don't control.").with_mana_cost(cost("{1}{G}")).id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::Green]);
    runner
        .cast(spell)
        .target_objects(&[target, foreign])
        .resolve();
    assert_eq!(pt(&mut runner, target), (3, 6));
    assert_eq!(runner.state().objects[&target].damage_marked, 2);
    assert_eq!(runner.state().objects[&foreign].damage_marked, 3);
    assert_eq!(pt(&mut runner, decoy), (8, 8));
    settled(&runner);
}

#[test]
fn walking_sponge_actual_named_choice_removes_only_nondefault_trample() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Walking Sponge", 1, 1,
        "{T}: Target creature loses your choice of flying, first strike, or trample until end of turn.").id();
    let target = scenario
        .add_creature(P1, "Three keywords", 3, 3)
        .flying()
        .first_strike()
        .trample()
        .id();
    let mut runner = scenario.build();
    runner.activate(source, 0).target_object(target).resolve();
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!("actual ability-removal choice");
    };
    assert_eq!(*player, P0);
    let choice = options
        .iter()
        .find(|value| value.eq_ignore_ascii_case("trample"))
        .expect("Trample option")
        .clone();
    assert_ne!(options[0], choice);
    runner.act(GameAction::ChooseOption { choice }).unwrap();
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Trample));
    for keyword in [Keyword::Flying, Keyword::FirstStrike] {
        assert!(runner.state().objects[&target].keywords.contains(&keyword));
    }
    assert!(runner.state().objects[&source].tapped);
    settled(&runner);
    runner.advance_to_upkeep();
    assert!(runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Trample));
}

#[test]
fn exile_hand_land_grant_gaps_preserve_paid_cost_and_independent_cast_permission() {
    for (name, oracle, mana, payment, generic, colors) in [
        ("Emrakul, the Exigent Doom", "When you cast this spell, untap all lands you control.\nFlying, trample\nWard—Sacrifice three permanents.\n{3}, Exile this card from your hand: Target land gains \"{T}: Add {C}{C}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.", "{10}", 3, 10, vec![]),
        ("Glamorous Outlaw", "When this creature enters, it deals 2 damage to each opponent and you scry 2.\n{2}, Exile this card from your hand: Target land gains \"{T}: Add {U}, {B}, or {R}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.", "{3}{U}{B}{R}", 2, 3, vec![ManaType::Blue, ManaType::Black, ManaType::Red]),
        ("Masked Bandits", "Vigilance\nMenace (This creature can't be blocked except by two or more creatures.)\n{2}, Exile this card from your hand: Target land gains \"{T}: Add {B}, {R}, or {G}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.", "{3}{B}{R}{G}", 2, 3, vec![ManaType::Black, ManaType::Red, ManaType::Green]),
        ("Rakish Revelers", "When this creature enters, create a 1/1 green and white Citizen creature token.\n{2}, Exile this card from your hand: Target land gains \"{T}: Add {R}, {G}, or {W}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.", "{2}{R}{G}{W}", 2, 2, vec![ManaType::Red, ManaType::Green, ManaType::White]),
        ("Shattered Seraph", "Flying\nWhen this creature enters, you gain 3 life.\n{2}, Exile this card from your hand: Target land gains \"{T}: Add {W}, {U}, or {B}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.", "{4}{W}{U}{B}", 2, 4, vec![ManaType::White, ManaType::Blue, ManaType::Black]),
        ("Spara's Adjudicators", "When this creature enters, target creature an opponent controls can't attack or block until your next turn.\n{2}, Exile this card from your hand: Target land gains \"{T}: Add {G}, {W}, or {U}\" until this card is cast from exile. You may cast this card for as long as it remains exiled.", "{2}{G}{W}{U}", 2, 2, vec![ManaType::Green, ManaType::White, ManaType::Blue]),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Scry first", "Scry second"]);
        let land = scenario.add_basic_land(P0, ManaColor::Green);
        let other = scenario.add_basic_land(P0, ManaColor::Blue);
        let victim = scenario.add_creature(P1, "Adjudicators real ETB target", 2, 3).id();
        let face = printed(name);
        let power = face["power"].as_str().unwrap().parse().unwrap();
        let toughness = face["toughness"].as_str().unwrap().parse().unwrap();
        let source = scenario.add_creature_to_hand_from_oracle(P0, name, power, toughness, oracle).with_mana_cost(cost(mana)).id();
        let mut runner = scenario.build();
        let index = engine::game::casting::activated_ability_definitions(runner.state(), source)[0].0;
        let land_abilities = runner.state().objects[&land].abilities.len();
        let other_abilities = runner.state().objects[&other].abilities.len();
        fund(&mut runner, payment, &[]);
        runner.activate(source, index).target_object(land).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().objects[&land].abilities.len(), land_abilities, "an honest unsupported whole grant cannot invent another duration");
        assert_eq!(runner.state().objects[&other].abilities.len(), other_abilities);
        settled(&runner);
        fund(&mut runner, generic, &colors);
        runner.cast(source).target_object(victim).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield, "independent while-exiled permission survives the grant gap");
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        settled(&runner);
    }
}

#[test]
fn revenge_keeps_both_pump_and_trample_on_the_nondefault_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "Unselected", 2, 2).id();
    let target = scenario.add_creature(P0, "Selected", 3, 3).id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Revenge of the Hunted", false,
        "Until end of turn, target creature gets +6/+6 and gains trample, and all creatures able to block it this turn do so.\nMiracle {G} (You may cast this card for its miracle cost when you draw it if it's the first card you drew this turn.)").with_mana_cost(cost("{4}{G}{G}")).id();
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Green; 2]);
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (9, 9));
    assert!(runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Trample));
    assert_eq!(pt(&mut runner, first), (2, 2));
    settled(&runner);
    runner.advance_to_upkeep();
    assert_eq!(pt(&mut runner, target), (3, 3));
    assert!(!runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Trample));
}

#[test]
fn liara_combat_uses_own_battlefield_and_graveyard_names_but_not_opponents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Liara of the Flaming Fist", 2, 2,
        "At the beginning of each combat, each creature you control gets +1/+1 until end of turn if it has the same name as another creature you control or a creature card in your graveyard.\n{1}{R}{W}: Another target nontoken creature you control gains first strike and double team until end of turn. Activate only as a sorcery and only once.").id();
    let first = scenario.add_creature(P0, "Own pair", 2, 2).id();
    let second = scenario.add_creature(P0, "Own pair", 2, 2).id();
    let gy_match = scenario.add_creature(P0, "Own graveyard match", 3, 3).id();
    scenario.add_creature_to_graveyard(P0, "Own graveyard match", 3, 3);
    let foreign_match = scenario.add_creature(P0, "Only foreign match", 4, 4).id();
    scenario.add_creature(P1, "Only foreign match", 4, 4);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner.advance_until_stack_empty();
    for id in [first, second] {
        assert_eq!(pt(&mut runner, id), (3, 3));
    }
    assert_eq!(pt(&mut runner, gy_match), (4, 4));
    assert_eq!(pt(&mut runner, foreign_match), (4, 4));
    assert_eq!(pt(&mut runner, source), (2, 2));
}

#[test]
fn martyrdom_granted_redirect_keeps_original_authorized_activator_and_recipient() {
    use engine::game::engine::apply;
    use engine::types::game_state::CastPaymentMode;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grantee = scenario.add_creature(P0, "Redirect grantee", 2, 6).id();
    let other = scenario.add_creature(P1, "Exchange partner", 3, 7).id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Martyrdom", true,
        "Until end of turn, target creature you control gains \"{0}: The next 1 damage that would be dealt to target creature, planeswalker, or player this turn is dealt to this creature instead.\" Only you may activate this ability.").with_mana_cost(cost("{1}{W}{W}")).id();
    let exchange = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Switcheroo",
            false,
            "Exchange control of two target creatures.",
        )
        .with_mana_cost(cost("{4}{U}"))
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(cost("{R}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::White; 2]);
    runner.cast(spell).target_object(grantee).resolve();
    fund(&mut runner, 4, &[ManaType::Blue]);
    runner
        .cast(exchange)
        .target_objects(&[grantee, other])
        .resolve();
    assert_eq!(runner.state().objects[&grantee].controller, P1);
    let index = engine::game::casting::activated_ability_definitions(runner.state(), grantee)[0].0;
    let action = GameAction::ActivateAbility {
        source_id: grantee,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    };
    assert!(
        apply(runner.state_mut(), P1, action.clone()).is_err(),
        "new controller is not original only-you activator"
    );
    apply(runner.state_mut(), P0, action).unwrap();
    let WaitingFor::TargetSelection { player, .. } = &runner.state().waiting_for else {
        panic!("actual granted redirect target prompt");
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P0)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    fund(&mut runner, 0, &[ManaType::Red]);
    runner.cast(shock).target_player(P0).resolve();
    assert_eq!(runner.life(P0), 19);
    assert_eq!(runner.state().objects[&grantee].damage_marked, 1);
    assert_eq!(runner.state().objects[&other].damage_marked, 0);
    settled(&runner);
}

#[test]
fn explore_return_twice_and_paid_x_use_the_actual_recipient_and_full_count() {
    for returning in [false, true] {
        let name = if returning {
            "Defossilize"
        } else {
            "Jadelight Spelunker"
        };
        let face = printed(name);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let subject = if returning {
            scenario
                .add_creature_to_graveyard(P0, "Returned recipient", 2, 3)
                .id()
        } else {
            ObjectId(0)
        };
        let decoy = scenario.add_creature(P0, "Unrelated recipient", 3, 4).id();
        let mut library = vec![];
        for i in 0..3 {
            library.push(
                scenario
                    .add_spell_to_library_top(P0, &format!("Explore nonland {i}"), true)
                    .id(),
            );
        }
        library.reverse();
        let source = if returning {
            scenario
                .add_spell_to_hand_from_oracle(P0, name, false, face["text"].as_str().unwrap())
                .with_mana_cost(cost(face["manaCost"].as_str().unwrap()))
                .id()
        } else {
            scenario
                .add_creature_to_hand_from_oracle(P0, name, 1, 1, face["text"].as_str().unwrap())
                .with_mana_cost(cost(face["manaCost"].as_str().unwrap()))
                .id()
        };
        let mut runner = scenario.build();
        if returning {
            fund(&mut runner, 4, &[ManaType::Black]);
            runner.cast(source).target_object(subject).resolve();
        } else {
            fund(&mut runner, 3, &[ManaType::Green]);
            runner.cast(source).x(3).resolve();
        }
        let count = if returning { 2 } else { 3 };
        for id in library.iter().take(count) {
            let WaitingFor::DigChoice { player, cards, .. } = &runner.state().waiting_for else {
                panic!("each actual explore pauses on a nonland");
            };
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![*id]);
            runner
                .act(GameAction::SelectCards { cards: vec![*id] })
                .unwrap();
        }
        runner.advance_until_stack_empty();
        let recipient = if returning { subject } else { source };
        assert_eq!(runner.state().objects[&recipient].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&recipient]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&(count as u32))
        );
        assert!(!runner.state().objects[&decoy]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        settled(&runner);
    }
}

#[test]
fn explore_full_compass_activation_and_lantern_granted_attack_are_actions() {
    use engine::game::combat::AttackTarget;
    for equipment in [false, true] {
        let name = if equipment {
            "Glowcap Lantern"
        } else {
            "Guidestone Compass"
        };
        let face = printed(name);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P0, "Actual explorer", 2, 3).id();
        let decoy = scenario.add_creature(P0, "Unselected explorer", 3, 4).id();
        let card = scenario
            .add_spell_to_library_top(P0, "Explored nonland", true)
            .id();
        let source = scenario
            .add_artifact_from_oracle(P0, name, face["text"].as_str().unwrap())
            .id();
        let mut runner = scenario.build();
        fund(&mut runner, if equipment { 2 } else { 1 }, &[]);
        runner.activate(source, 0).target_object(target).resolve();
        if equipment {
            runner.advance_to_combat();
            runner.declare_attackers(vec![(target, AttackTarget::Player(P1))]);
            runner.advance_until_stack_empty();
        }
        let WaitingFor::DigChoice { player, cards, .. } = &runner.state().waiting_for else {
            panic!("full {name} generates a real Explore action");
        };
        assert_eq!(*player, P0);
        assert_eq!(cards, &vec![card]);
        runner
            .act(GameAction::SelectCards { cards: vec![card] })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&target]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&1)
        );
        assert!(!runner.state().objects[&source]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        assert!(!runner.state().objects[&decoy]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        settled(&runner);
    }
}

#[test]
fn topography_full_replacement_doubles_exploration_without_losing_its_map_entry() {
    let tracker_face = printed("Topography Tracker");
    let scout_face = printed("Cenote Scout");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tracker = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Topography Tracker",
            2,
            2,
            tracker_face["text"].as_str().unwrap(),
        )
        .with_mana_cost(cost(tracker_face["manaCost"].as_str().unwrap()))
        .id();
    let scout = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Cenote Scout",
            1,
            1,
            scout_face["text"].as_str().unwrap(),
        )
        .with_mana_cost(cost("{G}"))
        .id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second reveal", true)
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First reveal", true)
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::Green]);
    runner.cast(tracker).resolve();
    assert!(runner.state().objects.values().any(|object| object.is_token
        && object.zone == Zone::Battlefield
        && object.card_types.subtypes.contains(&"Map".into())));
    fund(&mut runner, 0, &[ManaType::Green]);
    runner.cast(scout).resolve();
    for expected in [first, second] {
        let WaitingFor::DigChoice { cards, .. } = &runner.state().waiting_for else {
            panic!("both replacement explores retain their choices");
        };
        assert_eq!(cards, &vec![expected]);
        runner
            .act(GameAction::SelectCards {
                cards: vec![expected],
            })
            .unwrap();
    }
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&scout]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2)
    );
    assert!(!runner.state().objects[&tracker]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
    settled(&runner);
}

#[test]
fn twists_actual_scry_replacement_precedes_explore_and_keeps_the_etb_recipient() {
    let face = printed("Twists and Turns");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Chosen explorer", 2, 3).id();
    let decoy = scenario.add_creature(P0, "Unselected explorer", 3, 4).id();
    let land = scenario
        .add_land_to_library_top(P0, "Actual explored land")
        .id();
    let bottomed = scenario
        .add_spell_to_library_top(P0, "Nonland scry to bottom", true)
        .id();
    let source = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Twists and Turns",
            false,
            face["text"].as_str().unwrap(),
        )
        .as_enchantment()
        .with_mana_cost(cost(face["manaCost"].as_str().unwrap()))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 0, &[ManaType::Green]);
    runner.cast(source).commit();
    runner.resolve_top();
    let WaitingFor::TriggerTargetSelection { player, .. } = &runner.state().waiting_for else {
        panic!("full Twists actual ETB target");
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(target)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let WaitingFor::ScryChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!("replacement scry before actual Explore");
    };
    assert_eq!(*player, P0);
    assert_eq!(cards, &vec![bottomed]);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&land].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&bottomed].zone, Zone::Library);
    assert!(!runner.state().objects[&target]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
    assert!(!runner.state().objects[&decoy]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
    settled(&runner);
}

#[test]
fn frog_does_not_force_a_devotion_god_to_remain_a_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oracle = "Indestructible\nAs long as your devotion to blue is less than five, Thassa isn't a creature. (Each {U} in the mana costs of permanents you control counts toward your devotion to blue.)\nAt the beginning of your upkeep, scry 1.\n{1}{U}: Target creature you control can't be blocked this turn.";
    let target = scenario
        .add_creature_from_oracle(P1, "Thassa, God of the Sea", 5, 5, oracle)
        .as_enchantment()
        .as_creature()
        .as_legendary()
        .with_subtypes(vec!["God"])
        .with_mana_cost(cost("{2}{U}"))
        .from_oracle_text(oracle)
        .id();
    let devotion = scenario
        .add_artifact_from_oracle(P1, "Four blue pips", "")
        .with_mana_cost(cost("{U}{U}{U}{U}"))
        .id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Turn to Frog", true,
        "Until end of turn, target creature loses all abilities and becomes a blue Frog with base power and toughness 1/1.").with_mana_cost(cost("{1}{U}")).id();
    let remove = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(cost("{1}{U}"))
        .id();
    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert!(runner.state().objects[&target]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    fund(&mut runner, 1, &[ManaType::Blue]);
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (1, 1));
    assert!(runner.state().objects[&target]
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    fund(&mut runner, 1, &[ManaType::Blue]);
    runner.cast(remove).target_object(devotion).resolve();
    assert!(!runner.state().objects[&target]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(runner.state().objects[&target]
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    assert!(runner.state().objects[&target]
        .card_types
        .subtypes
        .is_empty());
    settled(&runner);
}

#[test]
fn shay_keeps_each_represented_removal_and_its_independent_bounty_triggers() {
    use engine::types::keywords::{ProtectionTarget, WardCost};
    let face = printed("Shay Cormac");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Shay Cormac", 1, 1, face["text"].as_str().unwrap())
        .id();
    let target = scenario
        .add_creature(P1, "Three protected abilities", 3, 3)
        .hexproof()
        .indestructible()
        .with_keyword(Keyword::Shroud)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .with_keyword(Keyword::Protection(ProtectionTarget::CardType(
            "Creature".into(),
        )))
        .with_keyword(Keyword::Ward(WardCost::Mana(cost("{2}"))))
        .with_keyword(Keyword::Ward(WardCost::PayLife(3)))
        .id();
    let own = scenario
        .add_creature(P0, "Own protected creature", 4, 4)
        .hexproof()
        .indestructible()
        .with_keyword(Keyword::Shroud)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .with_mana_cost(cost("{R}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[]);
    runner.activate(source, 0).resolve();
    for keyword in [Keyword::Hexproof, Keyword::Indestructible, Keyword::Shroud] {
        assert!(!runner.state().objects[&target].keywords.contains(&keyword));
        assert!(runner.state().objects[&own].keywords.contains(&keyword));
    }
    assert!(
        !runner.state().objects[&target]
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Protection(_) | Keyword::Ward(_))),
        "both parameterizations of each kind are removed"
    );
    fund(&mut runner, 0, &[ManaType::Red]);
    runner.cast(bolt).target_object(target).resolve();
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2)
    );
    settled(&runner);
}

#[test]
fn tolaria_actual_upkeep_removes_banding_and_refuses_later_main_phase() {
    let face = printed("Tolaria");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let source = scenario
        .add_land_from_oracle(P0, "Tolaria", face["text"].as_str().unwrap())
        .as_legendary()
        .id();
    let target = scenario
        .add_creature(P1, "Banding recipient", 3, 3)
        .with_keyword(Keyword::Banding)
        .id();
    let mut runner = scenario.build();
    runner.activate(source, 1).target_object(target).resolve();
    assert!(!runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Banding));
    assert!(runner.state().objects[&source].tapped);
    settled(&runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.state_mut().objects.get_mut(&source).unwrap().tapped = false;
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 1,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto
            })
            .is_err(),
        "untapped source still obeys the upkeep-only restriction"
    );
}

#[test]
fn strength_snapshots_three_attackers_and_preserves_trample_until_cleanup() {
    use engine::game::combat::AttackTarget;
    let face = printed("Strength in Numbers");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P0, "Chosen attacking recipient", 2, 4)
        .id();
    let first = scenario.add_creature(P0, "Other attacker one", 1, 3).id();
    let second = scenario.add_creature(P0, "Other attacker two", 1, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Strength in Numbers",
            true,
            face["text"].as_str().unwrap(),
        )
        .with_mana_cost(cost("{1}{G}"))
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner.declare_attackers(vec![
        (target, AttackTarget::Player(P1)),
        (first, AttackTarget::Player(P1)),
        (second, AttackTarget::Player(P1)),
    ]);
    fund(&mut runner, 1, &[ManaType::Green]);
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (5, 7));
    assert!(runner.state().objects[&target]
        .keywords
        .contains(&Keyword::Trample));
    engine::game::zones::move_to_zone(runner.state_mut(), first, Zone::Hand, &mut vec![]);
    assert_eq!(
        pt(&mut runner, target),
        (5, 7),
        "later attacker departure cannot change the resolved value"
    );
    settled(&runner);
}

#[test]
fn alpine_moon_removes_land_subtypes_but_keeps_actual_legendary_artifact_axes() {
    use engine::types::card_type::Supertype;
    let moon_face = printed("Alpine Moon");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let named = scenario
        .add_land_from_oracle(
            P1,
            "Minamo, School at Water's Edge",
            "{T}: Add {U}.\n{U}, {T}: Untap target legendary permanent.",
        )
        .as_legendary()
        .id();
    let own = scenario
        .add_land_from_oracle(
            P0,
            "Minamo, School at Water's Edge",
            "{T}: Add {U}.\n{U}, {T}: Untap target legendary permanent.",
        )
        .as_legendary()
        .id();
    let coating = scenario.add_artifact_from_oracle(P0, "Liquimetal Coating", "{T}: Target permanent becomes an artifact in addition to its other types until end of turn.").id();
    let moon = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Alpine Moon",
            false,
            moon_face["text"].as_str().unwrap(),
        )
        .as_enchantment()
        .with_mana_cost(cost("{R}"))
        .id();
    let mut runner = scenario.build();
    runner.activate(coating, 0).target_object(named).resolve();
    assert!(runner.state().objects[&named]
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    fund(&mut runner, 0, &[ManaType::Red]);
    runner.cast(moon).commit();
    runner.resolve_top();
    let WaitingFor::NamedChoice { player, .. } = &runner.state().waiting_for else {
        panic!("real nonbasic land name choice");
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::ChooseOption {
            choice: "Minamo, School at Water's Edge".into(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let object = &runner.state().objects[&named];
    assert!(object.card_types.core_types.contains(&CoreType::Artifact));
    assert!(object.card_types.core_types.contains(&CoreType::Land));
    assert!(object.card_types.supertypes.contains(&Supertype::Legendary));
    assert!(object.card_types.subtypes.is_empty());
    assert_eq!(object.abilities.len(), 1);
    assert_eq!(
        runner.state().objects[&own].abilities.len(),
        2,
        "own named land keeps both original abilities"
    );
    settled(&runner);
}

#[test]
fn blink_natural_second_chapter_gives_the_created_token_its_own_removal_trigger() {
    let face = printed("Blink");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["First draw", "Second draw", "Third draw"]);
    scenario.with_library_top(P1, &["First draw", "Second draw", "Third draw"]);
    let shuffled = scenario
        .add_creature(P0, "Chapter-one recipient", 2, 3)
        .id();
    let unrelated = scenario
        .add_creature(P0, "Unrelated Alien Angel", 2, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Alien", "Angel"])
        .id();
    let friendly = scenario
        .add_creature_to_hand(P0, "Friendly creature", 1, 1)
        .with_mana_cost(cost("{1}"))
        .id();
    let foreign = scenario
        .add_creature_to_hand(P1, "Opposing flash creature", 1, 1)
        .flash()
        .with_mana_cost(cost("{1}"))
        .id();
    let source = scenario
        .add_spell_to_hand(P0, "Blink", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(cost("{2}{U}{B}"))
        .from_oracle_text(face["text"].as_str().unwrap())
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::Blue, ManaType::Black]);
    runner.cast(source).target_object(shuffled).resolve();
    assert_eq!(runner.state().objects[&shuffled].zone, Zone::Library);
    assert!(
        runner.state().objects.values().any(|object| object.is_token
            && object.zone == Zone::Battlefield
            && object.card_types.subtypes.contains(&"Clue".into())),
        "independent first chapter investigates"
    );
    for _ in 0..4 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_until_stack_empty();
        if runner.state().objects[&source]
            .counters
            .get(&CounterType::Lore)
            == Some(&2)
        {
            break;
        }
    }
    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Lore),
        Some(&2)
    );
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.contains(&"Alien".into())
                && object.card_types.subtypes.contains(&"Angel".into())
        })
        .map(|object| object.id)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    assert_eq!(runner.state().active_player, P0);
    fund(&mut runner, 1, &[]);
    runner.cast(friendly).resolve();
    assert!(
        runner.state().objects[&token]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "friendly creature spell does not remove the created token's type"
    );
    runner.act(GameAction::PassPriority).unwrap();
    runner.state_mut().players[1]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    runner.cast(foreign).resolve();
    assert_eq!(
        runner.state().objects[&token].card_types.core_types,
        vec![CoreType::Artifact]
    );
    assert!(runner.state().objects[&token]
        .card_types
        .subtypes
        .is_empty());
    assert!(runner.state().objects[&unrelated]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(runner.state().objects[&source]
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    settled(&runner);
}
