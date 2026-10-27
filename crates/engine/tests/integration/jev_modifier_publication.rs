//! Continuous instructions retain their complete subject, axes and duration.

use super::jev_defining_destroy::{cost, fund, settled};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
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
fn mystic_decree_paid_cast_removes_only_flying_and_islandwalk_until_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creatures = [P0, P1].map(|controller| {
        scenario
            .add_creature(controller, "Multiple distinct evasion abilities", 3, 4)
            .flying()
            .with_keyword(Keyword::Landwalk("Island".into()))
            .with_keyword(Keyword::Landwalk("Swamp".into()))
            .with_keyword(Keyword::Vigilance)
            .id()
    });
    // Complete pinned MTGJSON Oracle text and printed cost/type; this card is
    // outside the closed fixture consumed by printed().
    let decree = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Mystic Decree",
            false,
            "All creatures lose flying and islandwalk.",
        )
        .as_enchantment()
        .with_mana_cost(cost("{2}{U}{U}"))
        .with_color(vec![ManaColor::Blue])
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
    let object = runner.state_mut().objects.get_mut(&decree).unwrap();
    object.card_types.supertypes.push(Supertype::World);
    object.base_card_types = object.card_types.clone();
    for id in creatures {
        let keywords = &runner.state().objects[&id].keywords;
        for keyword in [
            Keyword::Flying,
            Keyword::Landwalk("Island".into()),
            Keyword::Landwalk("Swamp".into()),
            Keyword::Vigilance,
        ] {
            assert!(keywords.contains(&keyword));
        }
    }
    fund(&mut runner, 2, &[ManaType::Blue; 2]);
    runner.cast(decree).resolve();
    assert_eq!(runner.state().objects[&decree].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
    // CR 611.3a + CR 702.14a + CR 702.14c: all creatures lose the named
    // abilities, while a different landwalk quality and vigilance remain.
    for id in creatures {
        let keywords = &runner.state().objects[&id].keywords;
        assert!(!keywords.contains(&Keyword::Flying));
        assert!(!keywords.contains(&Keyword::Landwalk("Island".into())));
        assert!(keywords.contains(&Keyword::Landwalk("Swamp".into())));
        assert!(keywords.contains(&Keyword::Vigilance));
    }
    fund(&mut runner, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(decree).resolve();
    assert_eq!(runner.state().objects[&decree].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
    // CR 611.3b: the static effect ends when its source leaves the battlefield.
    for id in creatures {
        let keywords = &runner.state().objects[&id].keywords;
        for keyword in [
            Keyword::Flying,
            Keyword::Landwalk("Island".into()),
            Keyword::Landwalk("Swamp".into()),
            Keyword::Vigilance,
        ] {
            assert!(keywords.contains(&keyword));
        }
    }
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
    runner.declare_attackers(&[
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
    let blocker = scenario.add_creature(P1, "Required blocker", 1, 4).id();
    let second_blocker = scenario
        .add_creature(P1, "Other required blocker", 2, 4)
        .id();
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
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(target, engine::game::combat::AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_to_phase(Phase::DeclareBlockers);
    assert!(
        runner.declare_blockers(&[]).is_err(),
        "the printed lure instruction survives alongside the pump"
    );
    runner
        .declare_blockers(&[(blocker, target), (second_blocker, target)])
        .unwrap();
    runner.combat_damage();
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
fn martyrdom_complete_fixed_activator_gap_survives_a_real_paid_cast() {
    use engine::types::ability::Effect;

    let face = printed("Martyrdom");
    let oracle = face["text"].as_str().unwrap();
    assert_eq!(oracle, "Until end of turn, target creature you control gains \"{0}: The next 1 damage that would be dealt to target creature, planeswalker, or player this turn is dealt to this creature instead.\" Only you may activate this ability.");
    let parsed =
        engine::parser::parse_oracle_text(oracle, "Martyrdom", &[], &["Instant".into()], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    assert_eq!(
        parsed.abilities[0].effect.as_ref(),
        &Effect::unimplemented("fixed_grant_activator", oracle),
        "the complete duration, targeted quote and outside OnlyYou restriction form one gap"
    );
    assert!(parsed.abilities[0].sub_ability.is_none());
    assert!(parsed.statics.is_empty());
    assert!(parsed.replacements.is_empty());

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grantee = scenario
        .add_creature(P0, "Potential redirect grantee", 2, 6)
        .id();
    let other = scenario.add_creature(P1, "Unrelated creature", 3, 7).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Martyrdom", true, oracle)
        .with_mana_cost(cost(face["manaCost"].as_str().unwrap()))
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(cost("{R}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::White; 2]);
    // The targeted phrase is retained inside the unsupported atom; it does
    // not fabricate a supported target declaration or a granted activation.
    runner.cast(spell).resolve();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(
        engine::game::casting::activated_ability_definitions(runner.state(), grantee).is_empty()
    );
    assert!(engine::game::casting::activated_ability_definitions(runner.state(), other).is_empty());
    assert!(runner.state().transient_continuous_effects.is_empty());
    assert!(runner.state().pending_damage_replacements.is_empty());
    settled(&runner);

    fund(&mut runner, 0, &[ManaType::Red]);
    runner.cast(shock).target_player(P0).resolve();
    assert_eq!(
        runner.life(P0),
        18,
        "real damage has no fabricated redirection shield"
    );
    assert_eq!(runner.state().objects[&grantee].damage_marked, 0);
    assert_eq!(runner.state().objects[&other].damage_marked, 0);
    settled(&runner);
}

#[test]
fn ordinary_quoted_redirect_sibling_follows_the_recipients_current_controller() {
    use engine::game::engine::apply;
    use engine::types::game_state::CastPaymentMode;

    // Explicitly synthetic supported sibling: the ordinary quoted grant has
    // no outside OnlyYou restriction. It is not full Martyrdom support.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grantee = scenario.add_creature(P0, "Redirect grantee", 2, 6).id();
    let other = scenario.add_creature(P1, "Exchange partner", 3, 7).id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic ordinary redirect grant", true,
        "Until end of turn, target creature you control gains \"{0}: The next 1 damage that would be dealt to target creature, planeswalker, or player this turn is dealt to this creature instead.\"").with_mana_cost(cost("{1}{W}{W}")).id();
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
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(
        engine::game::casting::activated_ability_definitions(runner.state(), grantee).len(),
        1
    );
    fund(&mut runner, 4, &[ManaType::Blue]);
    runner
        .cast(exchange)
        .target_objects(&[grantee, other])
        .resolve();
    assert_eq!(runner.state().objects[&grantee].controller, P1);
    let definitions = engine::game::casting::activated_ability_definitions(runner.state(), grantee);
    assert_eq!(
        definitions.len(),
        1,
        "the ordinary grant survives the control exchange"
    );
    let index = definitions[0].0;
    let action = GameAction::ActivateAbility {
        source_id: grantee,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    };
    // CR 602.2: the recipient's current controller owns this ordinary
    // activation permission. Test the former controller while it has priority.
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(
        apply(runner.state_mut(), P0, action.clone()).is_err(),
        "the granting spell's controller cannot activate the opponent's ordinary granted ability"
    );
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    apply(runner.state_mut(), P1, action).unwrap();
    let WaitingFor::TargetSelection { player, .. } = &runner.state().waiting_for else {
        panic!("actual granted redirect target prompt");
    };
    assert_eq!(*player, P1);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P0)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "real activation creates the one-damage redirection shield"
    );
    settled(&runner);
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
            runner.declare_attackers(&[(target, AttackTarget::Player(P1))]);
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
    runner.declare_attackers(&[
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

// Complete pinned MTGJSON inputs, never expected parser output.
const STATIC_PRESERVATION_PRINTED: &[&str] = &[
    r###"{"name":"Awakener Druid","manaCost":"{2}{G}","manaValue":3.0,"type":"Creature — Human Druid","types":["Creature"],"subtypes":["Human","Druid"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"1","toughness":"1","layout":"normal","text":"When this creature enters, target Forest becomes a 4/5 green Treefolk creature for as long as this creature remains on the battlefield. It's still a land.","identifiers":{"scryfallOracleId":"7e8f5b34-fd16-4307-9104-17b584b25b14"}}"###,
    r###"{"name":"Halsin, Emerald Archdruid","manaCost":"{3}{G}","manaValue":4.0,"type":"Legendary Creature — Elf Druid","types":["Creature"],"subtypes":["Elf","Druid"],"supertypes":["Legendary"],"colors":["G"],"colorIdentity":["G"],"power":"2","toughness":"4","layout":"normal","text":"{1}: Until end of turn, target token you control becomes a green Bear creature with base power and toughness 4/4 in addition to its other colors and types.\nChoose a Background (You can have a Background as a second commander.)","keywords":["Choose a background"],"identifiers":{"scryfallOracleId":"01ab3603-6c2f-4d65-9bb2-da95efdbe622"}}"###,
    r###"{"name":"Relic's Roar","manaCost":"{U}","manaValue":1.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Until end of turn, target artifact or creature becomes a Dinosaur artifact creature with base power and toughness 4/3 in addition to its other types.","identifiers":{"scryfallOracleId":"7887f060-3024-4dc4-aeb5-fb5546db6b48"}}"###,
    r###"{"name":"Figure of Destiny","manaCost":"{R/W}","manaValue":1.0,"type":"Creature — Kithkin","types":["Creature"],"subtypes":["Kithkin"],"supertypes":[],"colors":["R","W"],"colorIdentity":["R","W"],"power":"1","toughness":"1","layout":"normal","text":"{R/W}: This creature becomes a Kithkin Spirit with base power and toughness 2/2.\n{R/W}{R/W}{R/W}: If this creature is a Spirit, it becomes a Kithkin Spirit Warrior with base power and toughness 4/4.\n{R/W}{R/W}{R/W}{R/W}{R/W}{R/W}: If this creature is a Warrior, it becomes a Kithkin Spirit Warrior Avatar with base power and toughness 8/8, flying, and first strike.","identifiers":{"scryfallOracleId":"cd51cc2d-0d39-4aba-b0ea-70e8118149bc"}}"###,
    r###"{"name":"Voldaren Bloodcaster // Bloodbat Summoner","faceName":"Bloodbat Summoner","side":"b","manaValue":2.0,"type":"Creature — Vampire Wizard","types":["Creature"],"subtypes":["Vampire","Wizard"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"3","layout":"transform","text":"Flying\nAt the beginning of combat on your turn, up to one target Blood token you control becomes a 2/2 black Bat creature with flying and haste in addition to its other types.","keywords":["Flying","Transform"],"identifiers":{"scryfallOracleId":"a720f3d3-c2d7-4e50-8cc6-600120380e9c"}}"###,
    r###"{"name":"Case of the Gorgon's Kiss","manaCost":"{B}","manaValue":1.0,"type":"Enchantment — Case","types":["Enchantment"],"subtypes":["Case"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"case","text":"When this Case enters, destroy up to one target creature that was dealt damage this turn.\nTo solve — Three or more creature cards were put into graveyards from anywhere this turn. (If unsolved, solve at the beginning of your end step.)\nSolved — This Case is a 4/4 Gorgon creature with deathtouch and lifelink in addition to its other types.","keywords":["Solved"],"identifiers":{"scryfallOracleId":"a89f9072-cf06-429f-a9b8-c11b5731ddd3"}}"###,
    r###"{"name":"Daxos's Torment","manaCost":"{3}{B}","manaValue":4.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Constellation — Whenever this enchantment or another enchantment you control enters, this enchantment becomes a 5/5 Demon creature with flying and haste in addition to its other types until end of turn.","keywords":["Constellation"],"identifiers":{"scryfallOracleId":"93ef3098-d1c0-4b05-9d28-1afa133bfc4f"}}"###,
    r###"{"name":"Reptilian Reflection","manaCost":"{2}{R}","manaValue":3.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Whenever you cycle a card, you may have this enchantment become a 5/4 Dinosaur creature with trample and haste in addition to its other types until end of turn.","identifiers":{"scryfallOracleId":"73f6619c-9d92-434e-9f06-52e0ff15cb5e"}}"###,
    r###"{"name":"Tan Jolom, the Worldwalker","manaCost":"{1}{G}{W}{U}","manaValue":4.0,"type":"Legendary Creature — Spirit","types":["Creature"],"subtypes":["Spirit"],"supertypes":["Legendary"],"colors":["G","U","W"],"colorIdentity":["G","U","W"],"power":"3","toughness":"3","layout":"normal","text":"At the beginning of combat on your turn, choose up to one target artifact or land you control that isn't a creature or token. Until end of turn, it becomes a 3/3 Spirit creature with vigilance and double team in addition to its other types.","identifiers":{"scryfallOracleId":"69bc6cdf-19f3-4c65-a422-8c7fb7350f82"}}"###,
    r###"{"name":"Shadow Puppeteers","manaCost":"{6}{U}","manaValue":7.0,"type":"Creature — Faerie Wizard","types":["Creature"],"subtypes":["Faerie","Wizard"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"4","toughness":"4","layout":"normal","text":"Flying, ward {2}\nWhen this creature enters, create two 1/1 black Faerie Rogue creature tokens with flying.\nWhenever a creature you control with flying attacks, you may have it become a red Dragon with base power and toughness 4/4 in addition to its other colors and types until end of turn.","keywords":["Flying","Ward"],"identifiers":{"scryfallOracleId":"0a570fdb-2104-4592-b158-5b5d88f1d087"}}"###,
    r###"{"name":"The Curse of Fenric","manaCost":"{2}{G}{W}","manaValue":4.0,"type":"Enchantment — Saga","types":["Enchantment"],"subtypes":["Saga"],"supertypes":[],"colors":["G","W"],"colorIdentity":["G","W"],"layout":"saga","text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — For each player, destroy up to one target creature that player controls. For each creature destroyed this way, its controller creates a 3/3 green Mutant creature token with deathtouch.\nII — Target nontoken creature becomes a 6/6 legendary Horror creature named Fenric and loses all abilities.\nIII — Target Mutant fights another target creature named Fenric.","keywords":["Fight"],"identifiers":{"scryfallOracleId":"ef7fce1b-0d95-4f92-a8ad-09d9756fa0e4"}}"###,
    r###"{"name":"Den of the Bugbear","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["R"],"layout":"normal","text":"If you control two or more other lands, this land enters tapped.\n{T}: Add {R}.\n{3}{R}: Until end of turn, this land becomes a 3/2 red Goblin creature with \"Whenever this creature attacks, create a 1/1 red Goblin creature token that's tapped and attacking.\" It's still a land.","identifiers":{"scryfallOracleId":"f451b8f0-1ff5-4e8d-9f30-9352d83ed687"}}"###,
    r###"{"name":"Raging Ravine","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["G","R"],"layout":"normal","text":"This land enters tapped.\n{T}: Add {R} or {G}.\n{2}{R}{G}: Until end of turn, this land becomes a 3/3 red and green Elemental creature with \"Whenever this creature attacks, put a +1/+1 counter on it.\" It's still a land.","identifiers":{"scryfallOracleId":"8d38194e-b607-4ff4-9c19-0e8636d463bf"}}"###,
    r###"{"name":"Great Hall of the Biblioplex","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{T}: Add {C}.\n{T}, Pay 1 life: Add one mana of any color. Spend this mana only to cast an instant or sorcery spell.\n{5}: If this land isn't a creature, it becomes a 2/4 Wizard creature with \"Whenever you cast an instant or sorcery spell, this creature gets +1/+0 until end of turn.\" It's still a land.","identifiers":{"scryfallOracleId":"a8c70dab-1e27-4a9c-bd2d-910d5720d02d"}}"###,
    r###"{"name":"Genju of the Fields","manaCost":"{W}","manaValue":1.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Enchant Plains\n{2}: Until end of turn, enchanted Plains becomes a 2/5 white Spirit creature with \"Whenever this creature deals damage, its controller gains that much life.\" It's still a land.\nWhen enchanted Plains is put into a graveyard, you may return this card from your graveyard to your hand.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"5cd35d18-115b-4d7c-8d05-3d4c8303f46e"}}"###,
    r###"{"name":"Svogthos, the Restless Tomb","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":["B","G"],"layout":"normal","text":"{T}: Add {C}.\n{3}{B}{G}: Until end of turn, this land becomes a black and green Plant Zombie creature with \"This creature's power and toughness are each equal to the number of creature cards in your graveyard.\" It's still a land.","identifiers":{"scryfallOracleId":"a34a70b8-02e5-4e8c-a9e7-b21c5a11dddf"}}"###,
    r###"{"name":"Indigo Faerie","manaCost":"{1}{U}","manaValue":2.0,"type":"Creature — Faerie Wizard","types":["Creature"],"subtypes":["Faerie","Wizard"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"1","layout":"normal","text":"Flying\n{U}: Target permanent becomes blue in addition to its other colors until end of turn.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"10ae611f-e56d-47c5-8b6e-d38d95208bba"}}"###,
    r###"{"name":"Prismatic Omen","manaCost":"{1}{G}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Lands you control are every basic land type in addition to their other types.","identifiers":{"scryfallOracleId":"d90882ae-6572-4214-8d9c-0bdfb75f9be2"}}"###,
    r###"{"name":"Dryad of the Ilysian Grove","manaCost":"{2}{G}","manaValue":3.0,"type":"Enchantment Creature — Nymph Dryad","types":["Enchantment","Creature"],"subtypes":["Nymph","Dryad"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"2","toughness":"4","layout":"normal","text":"You may play an additional land on each of your turns.\nLands you control are every basic land type in addition to their other types.","identifiers":{"scryfallOracleId":"bdbde5d0-f5e4-44da-b27c-b4ad6f374cc9"}}"###,
    r###"{"name":"Leyline of the Guildpact","manaCost":"{G/W}{G/U}{B/G}{R/G}","manaValue":4.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["B","G","R","U","W"],"colorIdentity":["B","G","R","U","W"],"layout":"normal","text":"If this card is in your opening hand, you may begin the game with it on the battlefield.\nEach nonland permanent you control is all colors.\nLands you control are every basic land type in addition to their other types.","identifiers":{"scryfallOracleId":"6b2e6a51-b9e6-4848-b7d7-98f02f2b897f"}}"###,
    r###"{"name":"Nylea's Presence","manaCost":"{1}{G}","manaValue":2.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Enchant land\nWhen this Aura enters, draw a card.\nEnchanted land is every basic land type in addition to its other types.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"93fe5eed-520f-467a-8e27-6f40eaa727e8"}}"###,
    r###"{"name":"Spreading Seas","manaCost":"{1}{U}","manaValue":2.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Enchant land\nWhen this Aura enters, draw a card.\nEnchanted land is an Island.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"af32757c-9d3c-4f6c-a6fa-d5729a440382"}}"###,
    r###"{"name":"Evil Presence","manaCost":"{B}","manaValue":1.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Enchant land\nEnchanted land is a Swamp.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"3d8ac41c-0566-48b2-a744-39db2f72272c"}}"###,
    r###"{"name":"Jetmir, Nexus of Revels","manaCost":"{1}{R}{G}{W}","manaValue":4.0,"type":"Legendary Creature — Cat Demon","types":["Creature"],"subtypes":["Cat","Demon"],"supertypes":["Legendary"],"colors":["G","R","W"],"colorIdentity":["G","R","W"],"power":"5","toughness":"4","layout":"normal","text":"Creatures you control get +1/+0 and have vigilance as long as you control three or more creatures.\nCreatures you control also get +1/+0 and have trample as long as you control six or more creatures.\nCreatures you control also get +1/+0 and have double strike as long as you control nine or more creatures.","identifiers":{"scryfallOracleId":"da72a4bc-ce6f-4b72-bc66-2ee33cfa87df"}}"###,
    r###"{"name":"Divine Sacrament","manaCost":"{1}{W}{W}","manaValue":3.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"White creatures get +1/+1.\nThreshold — White creatures get an additional +1/+1 as long as there are seven or more cards in your graveyard.","keywords":["Threshold"],"identifiers":{"scryfallOracleId":"92bd7fe2-bb70-40fb-984b-26e461347c56"}}"###,
    r###"{"name":"Nyxborn Hydra","manaCost":"{X}{G}","manaValue":1.0,"type":"Enchantment Creature — Hydra","types":["Enchantment","Creature"],"subtypes":["Hydra"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"0","toughness":"1","layout":"normal","text":"Bestow {X}{G}{G} (If you cast this card for its bestow cost, it's an Aura spell with enchant creature. It becomes a creature again if it's not attached.)\nReach, trample\nThis permanent enters with X +1/+1 counters on it.\nEnchanted creature gets +1/+1 for each +1/+1 counter on this Aura and has reach and trample.","keywords":["Bestow","Reach","Trample"],"identifiers":{"scryfallOracleId":"4d1b655f-05b8-4b82-9cb9-67660f570a9b"}}"###,
    r###"{"name":"Mace of the Valiant","manaCost":"{2}{W}","manaValue":3.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Equipped creature gets +1/+1 for each charge counter on this Equipment and has vigilance. (Attacking doesn't cause it to tap.)\nWhenever a creature you control enters, put a charge counter on this Equipment.\nEquip {3}","keywords":["Equip"],"identifiers":{"scryfallOracleId":"881c3dd4-49df-48c3-b514-509990332e25"}}"###,
    r###"{"name":"Reaper's Scythe","manaCost":"{2}{B}","manaValue":3.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"Job select\nAt the beginning of your end step, put a soul counter on this Equipment for each player who lost life this turn.\nEquipped creature gets +1/+1 for each soul counter on this Equipment and is an Assassin in addition to its other types.\nDeath Sickle — Equip {2}","keywords":["Death Sickle","Equip","Job select"],"identifiers":{"scryfallOracleId":"b0f7c2a9-914a-48c1-871c-6f72946d61d8"}}"###,
    r###"{"name":"Rakdos Riteknife","manaCost":"{2}","manaValue":2.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":["B","R"],"layout":"normal","text":"Equipped creature gets +1/+0 for each blood counter on this Equipment and has \"{T}, Sacrifice a creature: Put a blood counter on Rakdos Riteknife.\"\n{B}{R}, Sacrifice this Equipment: Target player sacrifices a permanent of their choice for each blood counter on this Equipment.\nEquip {2}","keywords":["Equip"],"identifiers":{"scryfallOracleId":"43a2bd01-3fce-4191-a08a-1e379c7770b0"}}"###,
    r###"{"name":"Ensoul Ring","manaCost":"{1}{U}","manaValue":2.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Enchant nonland permanent.\nEnchanted permanent is a Sol Ring. (It's no longer anything else.)","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"e0e9eaae-ecb0-4f2f-8f61-6a15319ebd36"}}"###,
    r###"{"name":"Lavinia, Azorius Renegade","manaCost":"{W}{U}","manaValue":2.0,"type":"Legendary Creature — Human Soldier","types":["Creature"],"subtypes":["Human","Soldier"],"supertypes":["Legendary"],"colors":["U","W"],"colorIdentity":["U","W"],"power":"2","toughness":"2","layout":"normal","text":"Each opponent can't cast noncreature spells with mana value greater than the number of lands that player controls.\nWhenever an opponent casts a spell, if no mana was spent to cast it, counter that spell.","identifiers":{"scryfallOracleId":"81ba3a33-0e04-4f68-9a42-1dd717733b57"}}"###,
    r###"{"name":"Pollywog Symbiote","manaCost":"{1}{U}","manaValue":2.0,"type":"Creature — Frog","types":["Creature"],"subtypes":["Frog"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"3","layout":"normal","text":"Each creature spell you cast costs {1} less to cast if it has mutate.\nWhenever you cast a creature spell, if it has mutate, draw a card, then discard a card.","identifiers":{"scryfallOracleId":"0a766368-c982-4cba-9638-c7a56978c240"}}"###,
    r###"{"name":"Slayer's Cleaver","manaCost":"{3}","manaValue":3.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"Equipped creature gets +3/+1 and must be blocked by an Eldrazi if able.\nEquip {4}","keywords":["Equip"],"identifiers":{"scryfallOracleId":"e51cbdce-017e-4b14-b3a7-2db50ddd8037"}}"###,
    r###"{"name":"Aspect of Wolf","manaCost":"{1}{G}","manaValue":2.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Enchant creature\nEnchanted creature gets +X/+Y, where X is half the number of Forests you control, rounded down, and Y is half the number of Forests you control, rounded up.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"77b7277d-90a1-4774-a998-8c35c3f94e4a"}}"###,
    r###"{"name":"Icingdeath, Frost Tyrant","manaCost":"{2}{W}{W}","manaValue":4.0,"type":"Legendary Creature — Dragon","types":["Creature"],"subtypes":["Dragon"],"supertypes":["Legendary"],"colors":["W"],"colorIdentity":["W"],"power":"4","toughness":"3","layout":"normal","text":"Flying, vigilance\nWhen Icingdeath, Frost Tyrant dies, create Icingdeath, Frost Tongue, a legendary white Equipment artifact token with \"Equipped creature gets +2/+0,\" \"Whenever equipped creature attacks, tap target creature defending player controls,\" and equip {2}.","keywords":["Flying","Vigilance"],"identifiers":{"scryfallOracleId":"1dc2c213-72f6-4112-96a8-bf6ad553c707"}}"###,
    r###"{"name":"Ashes of the Fallen","manaCost":"{2}","manaValue":2.0,"type":"Artifact","types":["Artifact"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"As this artifact enters, choose a creature type.\nEach creature card in your graveyard has the chosen creature type in addition to its other types.","identifiers":{"scryfallOracleId":"6437867b-a658-4a71-a8a5-0c5a782d1f84"}}"###,
    r###"{"name":"Psychic Paper","manaCost":"{2}","manaValue":2.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"As this Equipment becomes attached to a creature, choose a creature card name and a creature type.\nEquipped creature has ward {1}, it can't be blocked, and its name and creature type are the last chosen name and creature type.\nEquip {2}","keywords":["Equip"],"identifiers":{"scryfallOracleId":"110b601c-bf38-4985-8de6-fcb477f543a8"}}"###,
    r###"{"name":"Oppressive Rays","manaCost":"{W}","manaValue":1.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Enchant creature\nEnchanted creature can't attack or block unless its controller pays {3}.\nActivated abilities of enchanted creature cost {3} more to activate.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"4a17fb46-70db-4c9a-ac0e-dfc774345673"}}"###,
    r###"{"name":"Cowed by Wisdom","manaCost":"{W}","manaValue":1.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Enchant creature\nEnchanted creature can't attack or block unless its controller pays {1} for each card in your hand.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"10495c22-2456-4b93-afa9-0c3c53075efe"}}"###,
    r###"{"name":"Doc Aurlock, Grizzled Genius","manaCost":"{G}{U}","manaValue":2.0,"type":"Legendary Creature — Bear Druid","types":["Creature"],"subtypes":["Bear","Druid"],"supertypes":["Legendary"],"colors":["G","U"],"colorIdentity":["G","U"],"power":"2","toughness":"3","layout":"normal","text":"Spells you cast from your graveyard or from exile cost {2} less to cast.\nPlotting cards from your hand costs {2} less.","identifiers":{"scryfallOracleId":"3ca32660-bf5f-4398-bef6-a0ec8bc767cf"}}"###,
    r###"{"name":"Hearth Elemental // Stoke Genius","faceName":"Hearth Elemental","side":"a","manaCost":"{5}{R}","manaValue":6.0,"type":"Creature — Elemental","types":["Creature"],"subtypes":["Elemental"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"4","toughness":"5","layout":"adventure","text":"This spell costs {X} less to cast, where X is the number of cards in your graveyard that are instant cards, sorcery cards, and/or have an Adventure.","identifiers":{"scryfallOracleId":"f01cf0c5-0ac6-4ad3-8642-dd21c8e8563e"}}"###,
    r###"{"name":"Katilda, Dawnhart Martyr // Katilda's Rising Dawn","faceName":"Katilda's Rising Dawn","side":"b","manaValue":3.0,"type":"Legendary Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":["Legendary"],"colors":["W"],"colorIdentity":["W"],"layout":"transform","text":"Enchant creature\nEnchanted creature has flying, lifelink, and protection from Vampires, and it gets +X/+X, where X is the number of permanents you control that are Spirits and/or enchantments.\nIf Katilda's Rising Dawn would be put into a graveyard from anywhere, exile it instead.","keywords":["Disturb","Enchant","Flying","Lifelink","Protection","Transform"],"identifiers":{"scryfallOracleId":"493b2820-c250-48bd-9f3c-e1b5639ee101"}}"###,
    r###"{"name":"Think Twice","manaCost":"{1}{U}","manaValue":2.0,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Draw a card.\nFlashback {2}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)","keywords":["Flashback"],"identifiers":{"scryfallOracleId":"fa85c5a2-8e83-4624-a35a-a0bbf17ecbb4"}}"###,
    r###"{"name":"Gemrazer","manaCost":"{3}{G}","manaValue":4.0,"type":"Creature — Beast","types":["Creature"],"subtypes":["Beast"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"4","toughness":"4","layout":"mutate","text":"Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.","keywords":["Mutate","Reach","Trample"],"identifiers":{"scryfallOracleId":"3dfb0c0a-b68f-43b9-8475-28d0192fc4ed"}}"###,
    r###"{"name":"Tarelily, Lovestruck Archer","manaCost":"{1}{G}{W}","manaValue":3.0,"type":"Legendary Creature — Elf Archer","types":["Creature"],"subtypes":["Elf","Archer"],"supertypes":["Legendary"],"colors":["G","W"],"colorIdentity":["G","W"],"power":"2","toughness":"3","layout":"normal","text":"Elves and/or Dwarf spells you cast cost {1} less to cast.\nSoulbond (You may pair this creature with another unpaired creature when either enters. They remain paired for as long as you control both of them.)\nAs long as this creature is paired with another creature, both creatures have reach. If that creature is a Dwarf, both creatures have lifelink.","keywords":["Soulbond"],"identifiers":{"scryfallOracleId":"9f4cad96-079d-4791-9971-d7ebe43f5e04"}}"###,
    r###"{"name":"Airtight Alibi","manaCost":"{2}{G}","manaValue":3.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Flash\nEnchant creature\nWhen this Aura enters, untap enchanted creature. It gains hexproof until end of turn. If it's suspected, it's no longer suspected.\nEnchanted creature gets +2/+2 and can't become suspected.","keywords":["Enchant","Flash"],"identifiers":{"scryfallOracleId":"cb666ea8-55e3-4e27-ae6d-806677dfc17a"}}"###,
    r###"{"name":"Math is for Blockers","manaCost":"{4}{R}{R}","manaValue":6.0,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Damage can't be prevented this turn.\nCreatures you control have double strike, trample, and must attack if able until end of turn.","identifiers":{"scryfallOracleId":"1048c9f7-5b8f-4abd-be8c-881b58c97ec2"}}"###,
];

fn static_preservation_atomic(name: &str) -> engine::database::mtgjson::AtomicCard {
    STATIC_PRESERVATION_PRINTED
        .iter()
        .map(|record| {
            serde_json::from_str::<engine::database::mtgjson::AtomicCard>(record).unwrap()
        })
        .find(|card| card.face_name.as_deref().unwrap_or(&card.name) == name)
        .unwrap()
}

#[test]
fn static_preservation_complete_faces_survive_both_normal_loaders() {
    use engine::database::{
        synthesis::{build_oracle_face, build_oracle_face_multi},
        CardDatabase,
    };
    let mut export = serde_json::Map::new();
    for record in STATIC_PRESERVATION_PRINTED {
        let atomic: engine::database::mtgjson::AtomicCard = serde_json::from_str(record).unwrap();
        let face = if atomic.face_name.is_some() {
            build_oracle_face_multi(&atomic, atomic.identifiers.scryfall_oracle_id.clone())
        } else {
            build_oracle_face(&atomic, atomic.identifiers.scryfall_oracle_id.clone())
        };
        let mut value = serde_json::to_value(&face).unwrap();
        value["layout"] = serde_json::json!(atomic.layout);
        value["face_index"] = serde_json::json!(usize::from(atomic.side.as_deref() == Some("b")));
        export.insert(face.name.to_lowercase(), value);
    }
    let json = serde_json::to_string(&export).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &json).unwrap();
    for db in [
        CardDatabase::from_json_str(&json).unwrap(),
        CardDatabase::from_export(file.path()).unwrap(),
    ] {
        for record in STATIC_PRESERVATION_PRINTED {
            let atomic: engine::database::mtgjson::AtomicCard =
                serde_json::from_str(record).unwrap();
            let name = atomic.face_name.as_deref().unwrap_or(&atomic.name);
            let face = db.get_face_by_name(name).unwrap();
            assert_eq!(face.oracle_text.as_deref(), atomic.text.as_deref());
            assert!(
                !face.abilities.is_empty()
                    || !face.triggers.is_empty()
                    || !face.static_abilities.is_empty()
            );
            let value = serde_json::to_value(face).unwrap();
            let mut pending = vec![&value];
            let mut gaps = 0;
            while let Some(value) = pending.pop() {
                match value {
                    serde_json::Value::Object(fields) => {
                        gaps += usize::from(
                            fields.get("type").and_then(serde_json::Value::as_str)
                                == Some("Unimplemented"),
                        );
                        pending.extend(fields.values());
                    }
                    serde_json::Value::Array(elements) => pending.extend(elements),
                    _ => {}
                }
            }
            match name {
                "Ensoul Ring" | "Lavinia, Azorius Renegade" => assert!(
                    gaps > 0,
                    "unrepresented whole instruction remains honestly unsupported: {name}"
                ),
                // Independent older gaps are not a reason to publish an incorrect
                // one-shot static or to lose the now-correct cost filter.
                "Tarelily, Lovestruck Archer" => {
                    assert!(
                        gaps > 0,
                        "the partner's Dwarf condition must remain visible"
                    );
                    assert!(face.static_abilities.iter().any(|definition| {
                        matches!(
                            definition.affected,
                            engine::types::ability::TargetFilter::SourceOrPaired
                        ) && serde_json::to_string(&definition.modifications)
                            .unwrap()
                            .contains("Reach")
                    }));
                }
                "Divine Sacrament" => assert!(!face.abilities.iter().any(|definition| matches!(
                    definition.effect.as_ref(),
                    engine::types::ability::Effect::GenericEffect { .. }
                ))),
                "Pollywog Symbiote" => {
                    assert!(face.static_abilities.iter().any(|definition| matches!(&definition.mode,
                        engine::types::statics::StaticMode::ModifyCost { spell_filter: Some(filter), .. }
                        if serde_json::to_string(filter).unwrap().contains("HasKeywordKind"))));
                    assert!(!face.static_abilities.iter().any(|definition| matches!(
                        &definition.condition,
                        Some(
                            engine::types::ability::StaticCondition::RecipientMatchesFilter { .. }
                        )
                    )));
                }
                _ => assert_eq!(gaps, 0, "complete preservation input: {name}"),
            }
        }
    }
}

#[test]
fn static_preservation_figure_paid_conditional_forms_keep_complete_pt_and_keywords() {
    // CR 608.2c + CR 611.2a: the condition is checked on resolution and an
    // undurated form persists; subtype changes do not erase an artifact type.
    let atomic = static_preservation_atomic("Figure of Destiny");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let figure = scenario
        .add_creature_from_oracle(
            P0,
            "Figure of Destiny",
            1,
            1,
            atomic.text.as_deref().unwrap(),
        )
        .as_artifact_creature()
        .with_subtypes(vec!["Kithkin"])
        .id();
    let decoy = scenario.add_creature(P0, "Unmodified sibling", 2, 3).id();
    let mut runner = scenario.build();
    fund(&mut runner, 0, &[ManaType::Red; 3]);
    runner.activate(figure, 1).resolve();
    assert_eq!(pt(&mut runner, figure), (1, 1));
    settled(&runner);
    for (index, paid, expected) in [(0, 1, (2, 2)), (1, 3, (4, 4)), (2, 6, (8, 8))] {
        fund(&mut runner, 0, &vec![ManaType::Red; paid]);
        runner.activate(figure, index).resolve();
        assert_eq!(pt(&mut runner, figure), expected);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        settled(&runner);
    }
    let figure_object = &runner.state().objects[&figure];
    assert!(figure_object.keywords.contains(&Keyword::Flying));
    assert!(figure_object.keywords.contains(&Keyword::FirstStrike));
    assert!(figure_object
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert_eq!(pt(&mut runner, decoy), (2, 3));
    runner.advance_to_upkeep();
    assert_eq!(pt(&mut runner, figure), (8, 8));
    settled(&runner);
}

#[test]
fn static_preservation_relic_paid_animation_keeps_base_pt_and_other_types_until_cleanup() {
    // CR 205.1b + CR 611.2a: retained types and base P/T are independent axes.
    let atomic = static_preservation_atomic("Relic's Roar");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P1, "Artifact enchantment recipient", 1, 6)
        .as_enchantment()
        .as_artifact_creature()
        .with_subtypes(vec!["Elf"])
        .flying()
        .id();
    let decoy = scenario.add_creature(P1, "Unselected recipient", 5, 5).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Relic's Roar", true, atomic.text.as_deref().unwrap())
        .with_mana_cost(cost(atomic.mana_cost.as_deref().unwrap()))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 0, &[ManaType::Blue]);
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (4, 3));
    let object = &runner.state().objects[&target];
    for core in [
        CoreType::Artifact,
        CoreType::Creature,
        CoreType::Enchantment,
    ] {
        assert!(object.card_types.core_types.contains(&core));
    }
    assert!(object.card_types.subtypes.contains(&"Elf".into()));
    assert!(object.card_types.subtypes.contains(&"Dinosaur".into()));
    assert!(object.keywords.contains(&Keyword::Flying));
    assert_eq!(pt(&mut runner, decoy), (5, 5));
    settled(&runner);
    runner.advance_to_upkeep();
    assert_eq!(pt(&mut runner, target), (1, 6));
    assert!(!runner.state().objects[&target]
        .card_types
        .subtypes
        .contains(&"Dinosaur".into()));
    settled(&runner);
}

#[test]
fn static_preservation_den_paid_animation_publishes_real_attack_trigger_and_retains_artifact_land()
{
    use engine::game::combat::AttackTarget;
    let atomic = static_preservation_atomic("Den of the Bugbear");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let den = scenario
        .add_land_from_oracle(P0, "Den of the Bugbear", atomic.text.as_deref().unwrap())
        .as_artifact_land()
        .id();
    let decoy = scenario
        .add_land_from_oracle(P0, "Unanimated artifact land", "{T}: Add {C}.")
        .as_artifact_land()
        .id();
    scenario.with_library_top(P0, &["draw buffer"]);
    scenario.with_library_top(P1, &["draw buffer"]);
    let mut runner = scenario.build();
    fund(&mut runner, 3, &[ManaType::Red]);
    runner.activate(den, 1).resolve();
    assert_eq!(pt(&mut runner, den), (3, 2));
    for core in [CoreType::Artifact, CoreType::Land, CoreType::Creature] {
        assert!(runner.state().objects[&den]
            .card_types
            .core_types
            .contains(&core));
    }
    assert!(!runner.state().objects[&decoy]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(den, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_until_stack_empty();
    let goblins: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.contains(&"Goblin".into())
        })
        .collect();
    assert_eq!(
        goblins.len(),
        1,
        "the real granted trigger creates its token"
    );
    assert_eq!(goblins[0].controller, P0);
    assert!(goblins[0].tapped);
    assert!(runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|attacker| attacker.object_id == goblins[0].id));
    runner.advance_to_upkeep();
    let object = &runner.state().objects[&den];
    assert!(object.card_types.core_types.contains(&CoreType::Artifact));
    assert!(object.card_types.core_types.contains(&CoreType::Land));
    assert!(!object.card_types.core_types.contains(&CoreType::Creature));
    settled(&runner);
}

#[test]
fn static_preservation_jetmir_three_live_thresholds_have_independent_complete_grants() {
    let atomic = static_preservation_atomic("Jetmir, Nexus of Revels");
    for count in [2, 3, 6, 9] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P0, "Measured teammate", 2, 3).id();
        for _ in 2..count {
            scenario.add_creature(P0, "Threshold teammate", 1, 1);
        }
        let foreign = scenario.add_creature(P1, "Opponent's creature", 2, 3).id();
        let jetmir = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Jetmir, Nexus of Revels",
                5,
                4,
                atomic.text.as_deref().unwrap(),
            )
            .with_mana_cost(cost(atomic.mana_cost.as_deref().unwrap()))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .with_mana_cost(cost("{U}"))
            .id();
        let mut runner = scenario.build();
        fund(
            &mut runner,
            1,
            &[ManaType::Red, ManaType::Green, ManaType::White],
        );
        runner.cast(jetmir).resolve();
        let before = i32::from(count >= 3) + i32::from(count >= 6) + i32::from(count >= 9);
        assert_eq!(pt(&mut runner, target), (2 + before, 3));
        let keywords = &runner.state().objects[&target].keywords;
        assert_eq!(keywords.contains(&Keyword::Vigilance), count >= 3);
        assert_eq!(keywords.contains(&Keyword::Trample), count >= 6);
        assert_eq!(keywords.contains(&Keyword::DoubleStrike), count >= 9);
        assert_eq!(pt(&mut runner, foreign), (2, 3));
        settled(&runner);
        fund(&mut runner, 0, &[ManaType::Blue]);
        runner.cast(bounce).target_object(jetmir).resolve();
        assert_eq!(pt(&mut runner, target), (2, 3));
        for keyword in [Keyword::Vigilance, Keyword::Trample, Keyword::DoubleStrike] {
            assert!(!runner.state().objects[&target].keywords.contains(&keyword));
        }
        settled(&runner);
    }
}

#[test]
fn static_preservation_basic_land_addition_and_setting_have_different_printed_ability_rules() {
    use engine::types::card_type::Supertype;
    for additive in [false, true] {
        let name = if additive {
            "Nylea's Presence"
        } else {
            "Spreading Seas"
        };
        let atomic = static_preservation_atomic(name);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario
            .add_land_from_oracle(
                P1,
                "Synthetic legendary artifact Locus",
                "{T}: Add {C}.\n{1}, {T}: Draw a card.",
            )
            .as_artifact_land()
            .as_legendary()
            .with_subtypes(vec!["Locus"])
            .id();
        let decoy = scenario
            .add_land_from_oracle(P1, "Unenchanted Locus", "{T}: Add {C}.")
            .with_subtypes(vec!["Locus"])
            .id();
        scenario.add_card_to_library_top(P0, "Actual Aura entry draw");
        let aura = scenario
            .add_spell_to_hand_from_oracle(P0, name, false, atomic.text.as_deref().unwrap())
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .with_mana_cost(cost(atomic.mana_cost.as_deref().unwrap()))
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
        fund(
            &mut runner,
            1,
            &[if additive {
                ManaType::Green
            } else {
                ManaType::Blue
            }],
        );
        runner
            .cast(aura)
            .target_object(target)
            .resolve()
            .assert_hand_drawn(P0, 1);
        engine::game::layers::evaluate_layers(runner.state_mut());
        let object = &runner.state().objects[&target];
        assert!(object.card_types.core_types.contains(&CoreType::Land));
        assert!(object.card_types.core_types.contains(&CoreType::Artifact));
        assert!(object.card_types.supertypes.contains(&Supertype::Legendary));
        assert!(!object.card_types.supertypes.contains(&Supertype::Basic));
        if additive {
            for subtype in ["Locus", "Plains", "Island", "Swamp", "Mountain", "Forest"] {
                assert!(object.card_types.subtypes.contains(&subtype.to_owned()));
            }
            assert!(
                object.abilities.len() >= 2,
                "CR 305.7: added land types retain printed abilities"
            );
        } else {
            assert_eq!(object.card_types.subtypes, vec!["Island".to_owned()]);
            assert_eq!(
                object.abilities.len(),
                1,
                "CR 305.7: setting a basic land type removes printed abilities"
            );
        }
        assert_eq!(
            runner.state().objects[&decoy].card_types.subtypes,
            vec!["Locus".to_owned()]
        );
        settled(&runner);
        fund(&mut runner, 1, &[ManaType::Blue]);
        runner.cast(bounce).target_object(aura).resolve();
        assert_eq!(
            runner.state().objects[&target].card_types.subtypes,
            vec!["Locus".to_owned()]
        );
        assert_eq!(runner.state().objects[&target].abilities.len(), 2);
        settled(&runner);
    }
}

#[test]
fn static_preservation_aspect_paid_aura_recomputes_distinct_rounded_axes() {
    let atomic = static_preservation_atomic("Aspect of Wolf");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Enchanted recipient", 2, 3).id();
    let decoy = scenario
        .add_creature(P0, "Unenchanted recipient", 4, 6)
        .id();
    let mut forests = Vec::new();
    for _ in 0..4 {
        forests.push(scenario.add_basic_land(P0, ManaColor::Green));
    }
    for _ in 0..7 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let aura = scenario
        .add_spell_to_hand_from_oracle(P0, "Aspect of Wolf", false, atomic.text.as_deref().unwrap())
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(cost("{1}{G}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Boomerang",
            true,
            "Return target permanent to its owner's hand.",
        )
        .with_mana_cost(cost("{U}{U}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 1, &[ManaType::Green]);
    runner.cast(aura).target_object(target).resolve();
    assert_eq!(pt(&mut runner, target), (4, 5));
    assert_eq!(pt(&mut runner, decoy), (4, 6));
    settled(&runner);
    fund(&mut runner, 0, &[ManaType::Blue; 2]);
    runner.cast(bounce).target_object(forests[0]).resolve();
    // CR 107.1a + CR 611.3a: three current own Forests give floor(3/2), ceil(3/2).
    assert_eq!(pt(&mut runner, target), (3, 5));
    assert_eq!(runner.state().objects[&forests[0]].zone, Zone::Hand);
    settled(&runner);
}

#[test]
fn static_preservation_icingdeath_real_death_token_keeps_quoted_pump_and_paid_equip() {
    let atomic = static_preservation_atomic("Icingdeath, Frost Tyrant");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(
            P0,
            "Icingdeath, Frost Tyrant",
            4,
            3,
            atomic.text.as_deref().unwrap(),
        )
        .id();
    let target = scenario
        .add_creature(P0, "Chosen equipment host", 2, 4)
        .id();
    let decoy = scenario
        .add_creature(P0, "Unchosen equipment host", 6, 6)
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
    fund(&mut runner, 0, &[ManaType::Red]);
    runner.cast(bolt).target_object(dragon).resolve();
    assert_eq!(runner.state().objects[&dragon].zone, Zone::Graveyard);
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.contains(&"Equipment".to_owned())
        })
        .map(|object| object.id)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "natural dies event creates the complete Equipment"
    );
    let equipment = tokens[0];
    settled(&runner);
    fund(&mut runner, 2, &[]);
    runner
        .act(GameAction::Equip {
            equipment_id: equipment,
            target_id: target,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(pt(&mut runner, target), (4, 4));
    assert_eq!(pt(&mut runner, decoy), (6, 6));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
}

#[test]
fn static_preservation_doc_origin_and_symbiote_keyword_reducers_read_the_cast_spell() {
    use engine::types::game_state::CastingVariant;
    for from_graveyard in [false, true] {
        let doc = static_preservation_atomic("Doc Aurlock, Grizzled Genius");
        let think = static_preservation_atomic("Think Twice");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(
            P0,
            "Doc Aurlock, Grizzled Genius",
            2,
            3,
            doc.text.as_deref().unwrap(),
        );
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Think Twice", true, think.text.as_deref().unwrap())
            .with_mana_cost(cost("{1}{U}"))
            .id();
        scenario.add_card_to_library_top(P0, "Actual drawn card");
        let mut runner = scenario.build();
        if from_graveyard {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                spell,
                Zone::Graveyard,
                &mut vec![],
            );
        }
        fund(&mut runner, usize::from(!from_graveyard), &[ManaType::Blue]);
        let cast = runner.cast(spell);
        if from_graveyard {
            cast.casting_variant(CastingVariant::Flashback)
                .resolve()
                .assert_hand_drawn(P0, 1);
        } else {
            cast.resolve().assert_hand_drawn(P0, 1);
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(
            runner.state().objects[&spell].zone,
            if from_graveyard {
                Zone::Exile
            } else {
                Zone::Graveyard
            }
        );
        settled(&runner);
    }
    let symbiote = static_preservation_atomic("Pollywog Symbiote");
    let gemrazer = static_preservation_atomic("Gemrazer");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Pollywog Symbiote",
        1,
        3,
        symbiote.text.as_deref().unwrap(),
    );
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, gemrazer.text.as_deref().unwrap())
        .from_oracle_text_with_keywords(
            &["Mutate", "Reach", "Trample"],
            gemrazer.text.as_deref().unwrap(),
        )
        .with_mana_cost(cost("{3}{G}"))
        .id();
    let discard = scenario.add_card_to_library_top(P0, "Actual drawn then discarded card");
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::Green]);
    runner.cast(spell).discard(&[discard]).resolve();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
}

#[test]
fn static_preservation_math_paid_mass_grant_keeps_keywords_requirement_and_cleanup() {
    use engine::game::combat::AttackTarget;
    let atomic = static_preservation_atomic("Math is for Blockers");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Required attacker", 2, 3).id();
    let opponent = scenario.add_creature(P1, "Unaffected opponent", 4, 5).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Math is for Blockers",
            true,
            atomic.text.as_deref().unwrap(),
        )
        .with_mana_cost(cost(atomic.mana_cost.as_deref().unwrap()))
        .id();
    scenario.with_library_top(P0, &["draw buffer"]);
    scenario.with_library_top(P1, &["draw buffer"]);
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Red; 2]);
    runner.cast(spell).resolve();
    for keyword in [Keyword::DoubleStrike, Keyword::Trample] {
        assert!(runner.state().objects[&own].keywords.contains(&keyword));
        assert!(!runner.state().objects[&opponent]
            .keywords
            .contains(&keyword));
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
    runner.advance_to_combat();
    assert!(
        runner.declare_attackers(&[]).is_err(),
        "printed must-attack member is enforced"
    );
    runner
        .declare_attackers(&[(own, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_to_upkeep();
    assert!(!runner.state().objects[&own]
        .keywords
        .contains(&Keyword::DoubleStrike));
    assert!(!runner.state().objects[&own]
        .keywords
        .contains(&Keyword::Trample));
    settled(&runner);
}
