//! #10130, slice 1: preserve the supertype qualifier in Detritivore's CDA.
//! Suspend X remains unsupported; the full card must retain exactly that gap.

use engine::game::coverage::card_face_gaps;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::ContinuousModification;
use engine::types::card::CardFace;
use engine::types::card_type::Supertype;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

// Verbatim Scryfall Oracle text, including the deliberately unsupported Suspend X line.
const DETRITIVORE: &str = "Detritivore's power and toughness are each equal to the number of nonbasic land cards in your opponents' graveyards.\nSuspend X—{X}{3}{R}. X can't be 0. (Rather than cast this card from your hand, you may pay {X}{3}{R} and exile it with X time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost. It has haste.)\nWhenever a time counter is removed from this card while it's exiled, destroy target nonbasic land.";
const CONSUMING_ABERRATION: &str = "Consuming Aberration's power and toughness are each equal to the number of cards in your opponents' graveyards.\nWhenever you cast a spell, each opponent reveals cards from the top of their library until they reveal a land card, then puts those cards into their graveyard.";

/// CR 604.3 + CR 613.4a: the P/T CDA is a dynamic layer-7a static.
fn has_dynamic_pt_static(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .static_definitions
        .as_slice()
        .iter()
        .flat_map(|def| def.modifications.iter())
        .any(|modification| {
            matches!(
                modification,
                ContinuousModification::SetDynamicPower { .. }
                    | ContinuousModification::SetDynamicToughness { .. }
            )
        })
}

/// CR 208.2 + CR 208.2a: Detritivore's printed */* is set by its CDA.
/// CR 400.1 + CR 404.1: each player has a separate graveyard.
/// CR 102.2 + CR 102.3: only opponents' graveyards contribute, not our own.
#[test]
fn issue_10130_detritivore_counts_only_opponents_nonbasic_land_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_land_to_graveyard(P1, "Wasteland");
    scenario.add_land_to_graveyard(P1, "Command Tower");
    let forest = scenario.add_land_to_graveyard(P1, "Forest").id();
    scenario.add_land_to_graveyard(P0, "Strip Mine");
    let detritivore = scenario
        .add_creature_from_oracle(P0, "Detritivore", 0, 0, DETRITIVORE)
        .id();
    let aberration = scenario
        .add_creature_from_oracle(P0, "Consuming Aberration", 0, 0, CONSUMING_ABERRATION)
        .id();
    let mut runner = scenario.build();

    // CR 205.4a: Basic is a supertype, not a land subtype or a card name.
    // The scenario builder gives lands no supertypes, so mark Forest explicitly.
    let forest = runner.state_mut().objects.get_mut(&forest).unwrap();
    forest.card_types.supertypes.push(Supertype::Basic);
    forest.base_card_types.supertypes.push(Supertype::Basic);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let pt = |id: ObjectId| {
        let obj = &runner.state().objects[&id];
        (obj.power, obj.toughness)
    };
    // Reach guard: all three opponent graveyard cards are visible to the count.
    assert_eq!(pt(aberration), (Some(3), Some(3)));
    assert_eq!(pt(detritivore), (Some(2), Some(2)));
    assert!(has_dynamic_pt_static(&runner, detritivore));
}

/// SHAPE: parsing the CDA must remove only static_structure, not the Suspend X gap.
#[test]
fn issue_10130_detritivore_keeps_exactly_suspend_x_unknown_gap() {
    let parsed = parse_oracle_text(
        DETRITIVORE,
        "Detritivore",
        &["Suspend".to_string()],
        &["Creature".to_string()],
        &["Lhurgoyf".to_string()],
    );
    let face = CardFace {
        name: "Detritivore".to_string(),
        oracle_text: Some(DETRITIVORE.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        keywords: parsed.extracted_keywords,
        replacements: parsed.replacements,
        parse_warnings: parsed.parse_warnings,
        ..Default::default()
    };
    let gaps = card_face_gaps(&face);
    println!("Detritivore gap handlers: {gaps:?}");
    assert_eq!(gaps, ["Effect:unknown"]);
}
