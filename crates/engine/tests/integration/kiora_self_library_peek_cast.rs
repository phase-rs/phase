//! Regression coverage for self-library "look ... cast from among them" chains.
//!
//! These tests exercise production Oracle parsing and the resolution-time cast
//! path. They distinguish that one-shot private-library flow from the durable
//! exile permission used by ordinary impulse-draw chains.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastFromZoneDriver, CastPermissionConstraint, Comparator, ControllerRef,
    Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KIORA: &str = "Vigilance, ward {3}\nWhenever you cast a Kraken, Leviathan, Octopus, or Serpent spell from your hand, look at the top X cards of your library, where X is that spell's mana value. You may cast a spell with mana value less than X from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const AETHERWORKS_MARVEL: &str = "Whenever a permanent you control is put into a graveyard, you get {E} (an energy counter).\n{T}, Pay six {E}: Look at the top six cards of your library. You may cast a spell from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const COSMIC_CUBE: &str = "Ward {2}\nWhenever you attack, look at the top six cards of your library. You may cast a spell from among them with mana value less than or equal to the greatest power among attacking creatures you control without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const BOBBLEHEAD: &str = "{T}: Add one mana of any color.\n{3}, {T}: Look at the top X cards of your library, where X is the number of Bobbleheads you control. You may cast a spell with mana value 3 or less from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.\n{3}, {T}: Create a colorless snow artifact token named Icy Manalith with \"{T}: Add one mana of any color.\"";
const SVELLA: &str = "{6}{R}{G}, {T}: Look at the top four cards of your library. You may cast a spell from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const VELOMACHUS: &str = "Flying, vigilance, haste\nWhenever Velomachus Lorehold attacks, look at the top seven cards of your library. You may cast an instant or sorcery spell with mana value less than or equal to Velomachus Lorehold's power from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const APEX: &str = "Exile the top seven cards of your library. Until end of turn, you may cast spells from among them.\nIf this spell was cast from your hand, add ten mana of any one color.";
const TALENT: &str = "Target opponent reveals the top seven cards of their library. You may cast an instant or sorcery spell from among them without paying its mana cost. Then that player puts the rest into their graveyard.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, you may cast up to two instant and/or sorcery spells from among the revealed cards instead of one.";
const JACE: &str = "Flying\nWhen Jace's Mindseeker enters, target opponent mills five cards. You may cast an instant or sorcery spell from among them without paying its mana cost.";
const SILENT_BLADE: &str = "Ninjutsu {4}{U}{B} ({4}{U}{B}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever this creature deals combat damage to a player, look at that player's hand. You may cast a spell from among those cards without paying its mana cost.";
const MINDCLAW_SHAMAN: &str = "When this creature enters, target opponent reveals their hand. You may cast an instant or sorcery spell from among those cards without paying its mana cost.";
const MINDLEECH_MASS: &str = "Trample\nWhenever this creature deals combat damage to a player, you may look at that player's hand. If you do, you may cast a spell from among those cards without paying its mana cost.";
const EPIC_EXPERIMENT: &str = "Exile the top X cards of your library. You may cast instant and sorcery spells with mana value X or less from among them without paying their mana costs. Then put all cards exiled this way that weren't cast into your graveyard.";
const COLLECTED_CONJURING: &str = "Exile the top six cards of your library. You may cast up to two sorcery spells with mana value 3 or less from among them without paying their mana costs. Put the exiled cards not cast this way on the bottom of your library in a random order.";
const HAZORET: &str = "Shuffle your library, then exile the top four cards. You may cast any number of spells with mana value 5 or less from among them without paying their mana costs. Lands you control don't untap during your next untap step.";
const PRIMEVAL_SPAWN: &str = "Vigilance, trample, lifelink\nWhen Primeval Spawn leaves the battlefield, exile the top ten cards of your library. You may cast any number of spells with total mana value 10 or less from among them without paying their mana costs.";
const CAPSTONE: &str = "Exile cards from the top of your library until you exile cards with total mana value 4 or greater. You may cast any number of spells from among them without paying their mana costs.";
const FOUNDING: &str = "Read ahead (Choose a chapter and start with that many lore counters. Add one after your draw step. Skipped chapters don't trigger. Sacrifice after III.)\nI — You may cast an instant or sorcery spell with mana value 1 or 2 from your hand without paying its mana cost.\nII — Target player mills four cards.\nIII — Exile target instant or sorcery card from your graveyard. Copy it. You may cast the copy.";

const MEETING_OF_THE_FIVE: &str = "Exile the top ten cards of your library. You may cast spells with exactly three colors from among them this turn. Add {W}{W}{U}{U}{B}{B}{R}{R}{G}{G}. Spend this mana only to cast spells with exactly three colors.";

fn parse(oracle: &str, name: &str, types: &[&str]) -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        oracle,
        name,
        &[],
        &types.iter().map(|ty| ty.to_string()).collect::<Vec<_>>(),
        &[],
    )
}

fn cast_from_zone_in(definition: &AbilityDefinition) -> Option<&Effect> {
    if matches!(definition.effect.as_ref(), Effect::CastFromZone { .. }) {
        return Some(definition.effect.as_ref());
    }
    definition
        .sub_ability
        .as_deref()
        .and_then(cast_from_zone_in)
}

fn parsed_cast_from_zone(parsed: &engine::parser::oracle::ParsedAbilities) -> &Effect {
    parsed
        .abilities
        .iter()
        .find_map(cast_from_zone_in)
        .or_else(|| {
            parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref())
                .find_map(cast_from_zone_in)
        })
        .expect("exact Oracle text must parse a real CastFromZone effect")
}

fn has_self_library_peek(definition: &AbilityDefinition) -> bool {
    matches!(
        definition.effect.as_ref(),
        Effect::Dig {
            player: TargetFilter::Controller,
            destination: None,
            keep_count: Some(0),
            reveal: false,
            source,
            ..
        } if source.is_library()
    ) || definition
        .sub_ability
        .as_deref()
        .is_some_and(has_self_library_peek)
}

#[test]
fn self_library_peek_casts_route_during_resolution() {
    for (name, oracle, types) in [
        ("Kiora, Sovereign of the Deep", KIORA, &["Creature"][..]),
        ("Aetherworks Marvel", AETHERWORKS_MARVEL, &["Artifact"][..]),
        ("Construct a Cosmic Cube", COSMIC_CUBE, &["Artifact"][..]),
        ("Perception Bobblehead", BOBBLEHEAD, &["Artifact"][..]),
        ("Svella, Ice Shaper", SVELLA, &["Creature"][..]),
        ("Velomachus Lorehold", VELOMACHUS, &["Creature"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        assert!(
            parsed.abilities.iter().any(has_self_library_peek)
                || parsed
                    .triggers
                    .iter()
                    .filter_map(|trigger| trigger.execute.as_deref())
                    .any(has_self_library_peek),
            "{name} must first parse its self-library Dig producer"
        );
        assert!(
            matches!(
                parsed_cast_from_zone(&parsed),
                Effect::CastFromZone {
                    driver: CastFromZoneDriver::DuringResolution,
                    ..
                }
            ),
            "{name} must use the one-shot DuringResolution driver"
        );
    }
}

#[test]
fn self_library_peek_constraints_are_retained() {
    let kiora = parse(KIORA, "Kiora, Sovereign of the Deep", &["Creature"]);
    assert!(matches!(
        parsed_cast_from_zone(&kiora),
        Effect::CastFromZone {
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LT,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::EventSource,
                    },
                },
            }),
            ..
        }
    ));

    let bobblehead = parse(BOBBLEHEAD, "Perception Bobblehead", &["Artifact"]);
    assert!(matches!(
        parsed_cast_from_zone(&bobblehead),
        Effect::CastFromZone {
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            }),
            ..
        }
    ));

    for (name, oracle, types, expected_constraint) in [
        (
            "Aetherworks Marvel",
            AETHERWORKS_MARVEL,
            &["Artifact"][..],
            false,
        ),
        ("Svella, Ice Shaper", SVELLA, &["Creature"][..], false),
        (
            "Construct a Cosmic Cube",
            COSMIC_CUBE,
            &["Artifact"][..],
            true,
        ),
        ("Velomachus Lorehold", VELOMACHUS, &["Creature"][..], true),
    ] {
        let parsed = parse(oracle, name, types);
        let Effect::CastFromZone { constraint, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert_eq!(
            constraint.is_some(),
            expected_constraint,
            "{name} constraint shape"
        );
    }
}

#[test]
fn non_library_peek_anaphors_stay_lingering_permissions() {
    for (name, oracle, types) in [
        ("Apex of Power", APEX, &["Sorcery"][..]),
        ("Talent of the Telepath", TALENT, &["Sorcery"][..]),
        ("Jace's Mindseeker", JACE, &["Creature"][..]),
        ("Silent-Blade Oni", SILENT_BLADE, &["Creature"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        assert!(matches!(
            parsed_cast_from_zone(&parsed),
            Effect::CastFromZone {
                driver: CastFromZoneDriver::LingeringPermission,
                ..
            }
        ));
    }
}

#[test]
fn dig_peek_suffix_constraints_and_negative_siblings() {
    for (name, oracle, expected) in [
        ("Collected Conjuring", COLLECTED_CONJURING, 3),
        ("Hazoret's Undying Fury", HAZORET, 5),
    ] {
        let parsed = parse(oracle, name, &["Sorcery"]);
        assert!(matches!(
            parsed_cast_from_zone(&parsed),
            Effect::CastFromZone {
                constraint: Some(CastPermissionConstraint::ManaValue {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value },
                }),
                ..
            } if *value == expected
        ));
    }

    let epic = parse(EPIC_EXPERIMENT, "Epic Experiment", &["Sorcery"]);
    assert!(matches!(
        parsed_cast_from_zone(&epic),
        Effect::CastFromZone {
            driver: CastFromZoneDriver::LingeringPermission,
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
            }),
            ..
        } if name == "X"
    ));

    for (name, oracle) in [
        ("Primeval Spawn", PRIMEVAL_SPAWN),
        ("Improvisation Capstone", CAPSTONE),
        ("Founding the Third Path", FOUNDING),
    ] {
        let parsed = parse(oracle, name, &["Sorcery"]);
        let Effect::CastFromZone { constraint, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert!(
            constraint.is_none(),
            "{name} must not gain this suffix constraint"
        );
    }
}

fn reach_kiora_library_choice() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
    let legal = scenario
        .add_spell_to_library_top(P0, "Kiora Legal Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let rest = scenario
        .add_spell_to_library_top(P0, "Kiora Illegal Spell", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 2 life.")
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "Triggering Kraken", 2, 2)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, kraken, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(kraken).commit();
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Kiora's optional cast must succeed");
    (runner, legal, rest)
}

#[test]
fn kiora_accept_casts_during_resolution_and_bottoms_the_rest() {
    let (mut runner, legal, rest) = reach_kiora_library_choice();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Kiora must park the private library choice")
    };
    assert_eq!(zone, Zone::Library);
    assert_eq!(cards, vec![legal], "MV equal to X is not legal for Kiora");
    runner
        .act(GameAction::SelectCards { cards: vec![legal] })
        .expect("choosing Kiora's legal spell must succeed");
    assert_eq!(runner.state().objects[&legal].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
    assert!(
        runner.state().objects[&rest].casting_permissions.is_empty(),
        "the unchosen library card must not receive a cast permission"
    );
}

#[test]
fn kiora_decline_bottoms_every_looked_at_card_without_a_permission() {
    let (mut runner, legal, rest) = reach_kiora_library_choice();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Kiora decline must reach the private library choice")
    };
    assert_eq!(zone, Zone::Library);
    assert_eq!(cards, vec![legal]);

    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining Kiora's cast must succeed");

    assert_eq!(runner.state().objects[&legal].zone, Zone::Library);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
    assert!(
        runner.state().objects[&legal]
            .casting_permissions
            .is_empty()
            && runner.state().objects[&rest].casting_permissions.is_empty(),
        "declining the one-shot cast must leave no standing permission"
    );
}

#[test]
fn kiora_zero_eligible_cards_bottom_without_parking_a_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
    let equal_to_x = scenario
        .add_spell_to_library_top(P0, "Kiora Equal Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "One-Mana Triggering Kraken", 1, 1)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, kraken, false, vec![])],
    );

    let mut runner = scenario.build();
    runner.cast(kraken).commit();
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Kiora's optional cast must succeed");

    assert_eq!(
        runner.state().last_revealed_ids,
        vec![equal_to_x],
        "Kiora's look must run before the empty eligible pool auto-bottoms"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no legal MV < X spell must not open an empty choice"
    );
    assert_eq!(runner.state().objects[&equal_to_x].zone, Zone::Library);
    assert!(
        runner.state().objects[&equal_to_x]
            .casting_permissions
            .is_empty(),
        "an ineligible looked-at card must not receive a permission"
    );
}

fn reach_kiora_multi_candidate_choice() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
    let legal_one = scenario
        .add_spell_to_library_top(P0, "Kiora Legal One", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let legal_two = scenario
        .add_spell_to_library_top(P0, "Kiora Legal Two", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let equal_to_x = scenario
        .add_spell_to_library_top(P0, "Kiora Equal Spell", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 1 life.")
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "Triggering Kraken", 3, 3)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, kraken, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(kraken).commit();
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Kiora's optional cast must succeed");
    (runner, legal_one, legal_two, equal_to_x)
}

#[test]
fn kiora_multi_candidate_choice_casts_exactly_one_and_bottoms_the_rest() {
    let (mut runner, legal_one, legal_two, equal_to_x) = reach_kiora_multi_candidate_choice();
    let library_before = runner.state().players[P0.0 as usize].library.len();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("multiple eligible Kiora cards must reach the private choice")
    };
    assert_eq!(zone, Zone::Library);
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&legal_one) && cards.contains(&legal_two));
    assert!(!cards.contains(&equal_to_x));

    runner
        .act(GameAction::SelectCards {
            cards: vec![legal_one],
        })
        .expect("choosing one of Kiora's eligible spells must succeed");

    assert_eq!(runner.state().objects[&legal_one].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&legal_two].zone, Zone::Library);
    assert_eq!(runner.state().objects[&equal_to_x].zone, Zone::Library);
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 1,
        "exactly the selected spell leaves the looked-at library set"
    );
}

#[test]
fn kiora_bottom_order_is_deterministic_under_a_fixed_seed() {
    let run_once = || {
        let mut scenario = GameScenario::new_with_format(FormatConfig::standard(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
            .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
        for name in ["Kiora First", "Kiora Second", "Kiora Third"] {
            scenario
                .add_spell_to_library_top(P0, name, false)
                .with_mana_cost(ManaCost::generic(1))
                .from_oracle_text("You gain 1 life.");
        }
        let kraken = scenario
            .add_creature_to_hand(P0, "Three-Mana Triggering Kraken", 3, 3)
            .with_subtypes(vec!["Kraken"])
            .with_mana_cost(ManaCost::generic(3))
            .id();
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Colorless, kraken, false, vec![]))
                .collect(),
        );

        let mut runner = scenario.build();
        runner.cast(kraken).commit();
        runner.resolve_top();
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting Kiora's optional cast must succeed");
        let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
        else {
            panic!("three looked-at Kiora cards must reach the private choice")
        };
        assert_eq!(zone, Zone::Library);
        assert_eq!(cards.len(), 3);
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .expect("declining Kiora's cast must bottom the looked-at cards");
        runner.state().players[P0.0 as usize].library.clone()
    };

    assert_eq!(
        run_once(),
        run_once(),
        "the same seeded Kiora setup must randomize its bottom order deterministically"
    );
}

#[test]
fn svella_activated_peek_casts_one_spell_and_bottoms_the_rest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let svella = scenario
        .add_creature(P0, "Svella, Ice Shaper", 2, 4)
        .from_oracle_text(SVELLA)
        .id();
    let chosen = scenario
        .add_spell_to_library_top(P0, "Svella Chosen Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let rest = scenario
        .add_spell_to_library_top(P0, "Svella Rest Spell", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, svella, false, vec![]))
            .chain([
                ManaUnit::new(ManaType::Red, svella, false, vec![]),
                ManaUnit::new(ManaType::Green, svella, false, vec![]),
            ])
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.activate(svella, 0).accept_optional().resolve();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = outcome.final_waiting_for() else {
        panic!("Svella's activated ability must reach the library cast choice")
    };
    assert_eq!(*zone, Zone::Library);
    assert!(cards.contains(&chosen) && cards.contains(&rest));

    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("choosing Svella's free spell must succeed");
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
}

#[test]
fn perception_bobblehead_excludes_mana_value_four() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bobblehead = scenario
        .add_creature(P0, "Perception Bobblehead", 1, 1)
        .as_artifact()
        .with_subtypes(vec!["Bobblehead"])
        .from_oracle_text(BOBBLEHEAD)
        .id();
    scenario
        .add_creature(P0, "Perception Bobblehead", 1, 1)
        .as_artifact()
        .with_subtypes(vec!["Bobblehead"]);
    let mana_value_three = scenario
        .add_spell_to_library_top(P0, "Bobblehead MV3", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_four = scenario
        .add_spell_to_library_top(P0, "Bobblehead MV4", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, bobblehead, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let look_ability_index = runner.state().objects[&bobblehead]
        .abilities
        .iter()
        .position(|definition| matches!(definition.effect.as_ref(), Effect::Dig { .. }))
        .expect("the verbatim Bobblehead Oracle text must produce its look ability");
    let outcome = runner
        .activate(bobblehead, look_ability_index)
        .accept_optional()
        .resolve();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = outcome.final_waiting_for() else {
        panic!("Bobblehead's look must reach a library cast choice")
    };
    assert_eq!(*zone, Zone::Library);
    assert!(cards.contains(&mana_value_three));
    assert!(
        !cards.contains(&mana_value_four),
        "Bobblehead's fixed mana-value cap must exclude MV 4"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![mana_value_three],
        })
        .expect("choosing the MV 3 Bobblehead spell must succeed");
    assert_eq!(runner.state().objects[&mana_value_three].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&mana_value_four].zone, Zone::Library);
}

#[test]
fn velomachus_power_constraint_is_frozen_before_the_library_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let velomachus = scenario
        .add_creature(P0, "Velomachus Lorehold", 5, 5)
        .from_oracle_text_with_keywords(&["flying", "vigilance", "haste"], VELOMACHUS)
        .id();
    let mana_value_five = scenario
        .add_spell_to_library_top(P0, "Velomachus MV5", false)
        .with_mana_cost(ManaCost::generic(5))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_six = scenario
        .add_spell_to_library_top(P0, "Velomachus MV6", false)
        .with_mana_cost(ManaCost::generic(6))
        .from_oracle_text("You gain 1 life.")
        .id();
    for index in 0..5 {
        scenario
            .add_spell_to_library_top(P0, &format!("Velomachus Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(6))
            .from_oracle_text("You gain 1 life.");
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![velomachus],
        valid_attack_targets: vec![AttackTarget::Player(P1)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    runner
        .declare_attackers(&[(velomachus, AttackTarget::Player(P1))])
        .expect("Velomachus must be able to attack");
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Velomachus's optional cast must succeed");

    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Velomachus's attack trigger must reach the library cast choice")
    };
    assert_eq!(zone, Zone::Library);
    assert!(cards.contains(&mana_value_five));
    assert!(
        !cards.contains(&mana_value_six),
        "Velomachus at power 5 must exclude a mana-value 6 spell"
    );
}

#[test]
fn epic_experiment_freezes_x_for_the_lingering_cast_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let epic = scenario
        .add_spell_to_hand_from_oracle(P0, "Epic Experiment", false, EPIC_EXPERIMENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    let mana_value_two = scenario
        .add_spell_to_library_top(P0, "Epic MV2", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_three = scenario
        .add_spell_to_library_top(P0, "Epic MV3", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, epic, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner
        .cast(epic)
        .x(2)
        .accept_optional()
        .effect_zone(&[mana_value_two])
        .resolve();

    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::EffectZoneChoice { .. }
        ),
        "Epic's selected MV 2 spell must be accepted rather than leaving the cast choice parked"
    );
    assert_eq!(
        runner.state().objects[&mana_value_two].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&mana_value_three].zone,
        Zone::Graveyard
    );
}

/// Issue #6880: the "from among them" cast anaphor must carry the clause's
/// card-type restriction, not just its mana-value constraint.
///
/// CR 601.3: "A player can begin to cast a spell only if a rule or effect
/// allows that player to cast it." Velomachus Lorehold allows casting only *an
/// instant or sorcery spell*, so the type gate is part of the cast-legality
/// predicate exactly as much as the mana-value bound is. The parser bound the
/// permission to a bare `TargetFilter::ExiledBySource`, dropping the type leg
/// entirely — the mana-value ceiling survived as a `CastPermissionConstraint`
/// while any card type at or below that ceiling became castable.
///
/// The composed shape mirrors the already-correct
/// "from among cards exiled with [self]" sibling branch: the typed leg AND the
/// exile-set anaphor.
fn cast_target_of(oracle: &str, name: &str, types: &[&str]) -> TargetFilter {
    let parsed = parse(oracle, name, types);
    let Effect::CastFromZone { target, .. } = parsed_cast_from_zone(&parsed) else {
        unreachable!("helper returns CastFromZone")
    };
    target.clone()
}

#[test]
fn from_among_them_cast_retains_the_instant_or_sorcery_gate() {
    let instant_or_sorcery = TypeFilter::AnyOf(vec![TypeFilter::Instant, TypeFilter::Sorcery]);

    for (name, oracle, types) in [
        ("Velomachus Lorehold", VELOMACHUS, &["Creature"][..]),
        ("Jace's Mindseeker", JACE, &["Creature"][..]),
        ("Talent of the Telepath", TALENT, &["Sorcery"][..]),
    ] {
        let target = cast_target_of(oracle, name, types);
        let TargetFilter::And { filters } = &target else {
            panic!(
                "{name}: the cast permission must AND the card-type gate with the \
                 exile-set anaphor, got {target:?}"
            );
        };
        assert!(
            filters.contains(&TargetFilter::ExiledBySource),
            "{name}: the exile-set anaphor leg must survive the composition, got {filters:?}"
        );
        let typed = filters
            .iter()
            .find_map(|f| match f {
                TargetFilter::Typed(tf) => Some(tf),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name}: expected a typed leg, got {filters:?}"));
        assert_eq!(
            typed.type_filters,
            vec![instant_or_sorcery.clone()],
            "{name}: the clause restricts the cast to instant or sorcery spells"
        );
    }
}

/// Sibling guard: an *untyped* "cast a spell from among them" clause (Svella,
/// Aetherworks Marvel, Apex of Power, ... — the untyped majority of this
/// anaphor family) must keep its bare `ExiledBySource` binding. A type gate
/// synthesized where the Oracle text names no type would silently narrow every
/// one of those cards.
///
/// The last three rows are the hostile cases, and they are the point of this
/// test: Oracle text that carries a restrictive qualifier immediately next to
/// "spell"/"spells" which is NOT a card type. CR 601.3 does restrict those
/// casts, but along the color and mana-value axes — which this filter does not
/// model, and which `parse_cast_type_gate` must therefore never mistake for a
/// card type. Meeting of the Five ("spells with exactly three colors") probes
/// the color axis; Perception Bobblehead and Kiora ("a spell with mana value
/// N or less") probe the property axis that rides on
/// `CastPermissionConstraint` instead.
#[test]
fn untyped_from_among_them_cast_stays_a_bare_exile_anaphor() {
    for (name, oracle, types) in [
        ("Svella, Ice Shaper", SVELLA, &["Creature"][..]),
        ("Aetherworks Marvel", AETHERWORKS_MARVEL, &["Artifact"][..]),
        ("Apex of Power", APEX, &["Sorcery"][..]),
        ("Meeting of the Five", MEETING_OF_THE_FIVE, &["Sorcery"][..]),
        ("Perception Bobblehead", BOBBLEHEAD, &["Artifact"][..]),
        ("Kiora, Sovereign of the Deep", KIORA, &["Creature"][..]),
    ] {
        assert_eq!(
            cast_target_of(oracle, name, types),
            TargetFilter::ExiledBySource,
            "{name} names no card type, so its cast permission must stay unrestricted"
        );
    }
}

/// Extracts the three legs of a hand-bound cast permission.
///
/// The hand-bound branch composes its filter from two independent sources: the
/// prior `Effect::RevealHand` clause supplies the zone and the revealed player,
/// and the cast clause supplies the card type. A test that read only one leg
/// would pass while the branch silently dropped another, so every assertion
/// below reads all three.
fn hand_bound_cast_filter(oracle: &str, name: &str, types: &[&str]) -> TypedFilter {
    match cast_target_of(oracle, name, types) {
        TargetFilter::Typed(tf) => tf,
        other => panic!(
            "{name}: a hand-reveal chain must bind the cast to the revealed hand \
             as a single typed filter, got {other:?}"
        ),
    }
}

/// The hand-bound half of issue #6880, which the exile-bound tests above do not
/// reach: `chain_prior_hand_reveal_target` is set (no exile producer ever ran),
/// so the anaphor resolves against the revealed player's hand rather than
/// `ExiledBySource`, and the type gate has to be grafted onto that filter
/// instead of AND-ed with an exile anaphor.
///
/// Mindclaw Shaman is the only type-gated card in that family. Pre-fix the
/// branch emitted a bare `TypeFilter::Card`, so "an instant or sorcery spell"
/// reached every card in the revealed hand — a creature or land was castable
/// for free, contrary to CR 601.3.
///
/// The branch OVERWRITES `type_filters` rather than appending, so the bare
/// `Card` head noun must be gone, not merely accompanied. Asserting equality on
/// the whole vector (not `contains`) is what pins that.
#[test]
fn hand_bound_cast_retains_the_instant_or_sorcery_gate() {
    let typed = hand_bound_cast_filter(MINDCLAW_SHAMAN, "Mindclaw Shaman", &["Creature"]);

    assert_eq!(
        typed.type_filters,
        vec![TypeFilter::AnyOf(vec![
            TypeFilter::Instant,
            TypeFilter::Sorcery
        ])],
        "the clause restricts the cast to instant or sorcery spells, and replaces \
         the bare `Card` head noun rather than joining it"
    );
    assert_eq!(
        typed.controller,
        Some(ControllerRef::Opponent),
        "the candidate cards belong to the opponent who revealed, not the caster"
    );
    assert!(
        typed
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }),
        "the cards never left the revealed hand, so the zone leg must survive the \
         type graft, got {:?}",
        typed.properties
    );
}

/// Sibling guard for the untyped majority of the hand-bound family.
///
/// Silent-Blade Oni and Mindleech Mass say "cast a spell from among those
/// cards" — no card type is named, so the permission is unrestricted and the
/// filter must keep its bare `Card` head noun. Synthesizing a type gate here
/// would silently narrow both cards.
///
/// Their zone and controller legs are asserted for the same reason as above:
/// this test also has to fail if the type graft is generalized in a way that
/// clobbers the hand binding.
#[test]
fn untyped_hand_bound_cast_keeps_a_bare_card_filter() {
    for (name, oracle) in [
        ("Silent-Blade Oni", SILENT_BLADE),
        ("Mindleech Mass", MINDLEECH_MASS),
    ] {
        let typed = hand_bound_cast_filter(oracle, name, &["Creature"]);

        assert_eq!(
            typed.type_filters,
            vec![TypeFilter::Card],
            "{name} names no card type, so its cast permission must stay unrestricted"
        );
        assert_eq!(
            typed.controller,
            Some(ControllerRef::TriggeringPlayer),
            "{name} looks at the hand of the player it damaged"
        );
        assert!(
            typed
                .properties
                .contains(&FilterProp::InZone { zone: Zone::Hand }),
            "{name}: the cards stay in the looked-at hand, got {:?}",
            typed.properties
        );
    }
}

/// The user-visible half of issue #6880, driven through the real attack-trigger
/// resolution pipeline rather than asserted on the AST.
///
/// Velomachus has power 5. The looked-at cards include a mana-value 3 *creature*
/// — comfortably inside the mana-value ceiling but outside the "instant or
/// sorcery" permission. Pre-fix the engine offered it; CR 601.3 says it was
/// never a legal choice.
///
/// Reach guard against a vacuous negative: the same assertion block requires the
/// legal mana-value 4 sorcery to BE offered, proving the choice was actually
/// opened and populated rather than short-circuited to an empty prompt.
#[test]
fn velomachus_does_not_offer_a_creature_inside_its_mana_value_ceiling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let velomachus = scenario
        .add_creature(P0, "Velomachus Lorehold", 5, 5)
        .from_oracle_text_with_keywords(&["flying", "vigilance", "haste"], VELOMACHUS)
        .id();
    let legal_sorcery = scenario
        .add_spell_to_library_top(P0, "Velomachus Legal Sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    // The trap: mana value 3 <= Velomachus's power 5, so only the card-type
    // gate can exclude it.
    let creature_inside_ceiling = scenario
        .add_spell_to_library_top(P0, "Velomachus Trap Creature", false)
        .with_mana_cost(ManaCost::generic(3))
        .as_creature()
        .id();
    for index in 0..5 {
        scenario
            .add_spell_to_library_top(P0, &format!("Velomachus Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(6))
            .from_oracle_text("You gain 1 life.");
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![velomachus],
        valid_attack_targets: vec![AttackTarget::Player(P1)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    runner
        .declare_attackers(&[(velomachus, AttackTarget::Player(P1))])
        .expect("Velomachus must be able to attack");
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Velomachus's optional cast must succeed");

    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Velomachus's attack trigger must reach the library cast choice")
    };
    assert_eq!(zone, Zone::Library);
    assert!(
        cards.contains(&legal_sorcery),
        "reach guard: the legal instant-or-sorcery card must be offered, \
         otherwise the negative assertion below is vacuous; offered = {cards:?}"
    );
    assert!(
        !cards.contains(&creature_inside_ceiling),
        "CR 601.3: Velomachus permits casting only an instant or sorcery spell — \
         a creature within the mana-value ceiling must NEVER be offered \
         (issue #6880); offered = {cards:?}"
    );

    // Observable outcome: taking the only legal card puts it on the stack and
    // leaves the illegal creature in the library.
    runner
        .act(GameAction::SelectCards {
            cards: vec![legal_sorcery],
        })
        .expect("choosing Velomachus's legal sorcery must succeed");
    assert_eq!(runner.state().objects[&legal_sorcery].zone, Zone::Stack);
    assert_eq!(
        runner.state().objects[&creature_inside_ceiling].zone,
        Zone::Library,
        "the ineligible creature must stay in the library"
    );
    assert!(
        runner.state().objects[&creature_inside_ceiling]
            .casting_permissions
            .is_empty(),
        "the ineligible creature must not receive a casting permission"
    );
}

/// Runtime sibling guard for the untyped majority: Svella says "cast a spell",
/// so a creature in its looked-at set must STILL be offered. This is the
/// runtime counterpart of `untyped_from_among_them_cast_stays_a_bare_exile_anaphor`
/// and fails if the type gate is applied where no type was named.
#[test]
fn svella_untyped_peek_still_offers_a_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let svella = scenario
        .add_creature(P0, "Svella, Ice Shaper", 2, 4)
        .from_oracle_text(SVELLA)
        .id();
    let creature = scenario
        .add_spell_to_library_top(P0, "Svella Creature", false)
        .with_mana_cost(ManaCost::generic(2))
        .as_creature()
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, svella, false, vec![]))
            .chain([
                ManaUnit::new(ManaType::Red, svella, false, vec![]),
                ManaUnit::new(ManaType::Green, svella, false, vec![]),
            ])
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.activate(svella, 0).accept_optional().resolve();
    let WaitingFor::EffectZoneChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!("Svella's activated ability must reach the library cast choice")
    };
    assert!(
        cards.contains(&creature),
        "Svella's untyped \"cast a spell\" permission must still reach a creature; \
         offered = {cards:?}"
    );
}

#[test]
fn kiora_library_choice_is_private_across_serde_round_trip() {
    let (runner, legal, rest) = reach_kiora_library_choice();
    let controller = filter_state_for_viewer(runner.state(), P0);
    let opponent = filter_state_for_viewer(runner.state(), P1);
    let WaitingFor::EffectZoneChoice { cards, .. } = &controller.waiting_for else {
        panic!("controller must retain Kiora's library choice")
    };
    assert_eq!(cards, &vec![legal]);
    assert_eq!(controller.objects[&legal].name, "Kiora Legal Spell");
    let WaitingFor::EffectZoneChoice { cards, .. } = &opponent.waiting_for else {
        panic!("opponent still sees a redacted choice envelope")
    };
    assert!(cards.iter().all(|id| *id == ObjectId(0)));
    assert_eq!(opponent.objects[&legal].name, "Hidden Card");
    assert_eq!(opponent.objects[&rest].name, "Hidden Card");

    let restored: engine::types::game_state::GameState = serde_json::from_str(
        &serde_json::to_string(runner.state()).expect("parked state serializes"),
    )
    .expect("parked state deserializes");
    let restored_opponent = filter_state_for_viewer(&restored, P1);
    let WaitingFor::EffectZoneChoice { cards, .. } = &restored_opponent.waiting_for else {
        panic!("restored opponent view must retain redaction")
    };
    assert!(cards.iter().all(|id| *id == ObjectId(0)));
    assert_eq!(restored_opponent.objects[&legal].name, "Hidden Card");
}
