//! #10130, slice 1: preserve the supertype qualifier in Detritivore's CDA.
//! Suspend X remains unsupported; the full card must retain exactly that gap.

use engine::game::coverage::card_face_gaps;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect};
use engine::types::card::CardFace;
use engine::types::card_type::Supertype;
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

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

/// CR 102.3 + CR 810.1: P0 and P1 are teammates; P2 and P3 are opponents.
/// CR 109.5: Detritivore's static reads opponents of its current controller, P0.
#[test]
fn issue_10130_detritivore_two_headed_giant_counts_settled_opponent_graveyards() {
    const P2: PlayerId = PlayerId(2);
    const WASTELAND: &str =
        "{T}: Add {C}.\n{T}, Sacrifice this land: Destroy target nonbasic land.";
    const COMMAND_TOWER: &str =
        "{T}: Add one mana of any color in your commander's color identity.";

    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Keep the CDA at 1/1 before any action so normal state-based actions
    // cannot put a legitimate 0/0 Detritivore into the graveyard.
    let initial_opponent_land = scenario.add_land_to_graveyard(P2, "Command Tower").id();
    let teammate_land = scenario
        .add_land_from_oracle(P1, "Command Tower", COMMAND_TOWER)
        .id();
    let opponent_land = scenario
        .add_land_from_oracle(P2, "Command Tower", COMMAND_TOWER)
        .id();
    let first_wasteland = scenario
        .add_land_from_oracle(P0, "Wasteland", WASTELAND)
        .id();
    let second_wasteland = scenario
        .add_land_from_oracle(P0, "Wasteland", WASTELAND)
        .id();
    let detritivore = scenario
        .add_creature_from_oracle(P0, "Detritivore", 0, 0, DETRITIVORE)
        .id();
    let mut runner = scenario.build();

    // Both printed activated abilities occupy the normal ability vector:
    // index 0 adds mana; index 1 sacrifices Wasteland and destroys the target.
    // These actions pay costs, choose targets, and pass priority through the
    // production reducer, including its normal layer/SBA settlement.
    let teammate_outcome = runner
        .activate(first_wasteland, 1)
        .target_object(teammate_land)
        .resolve();
    assert_eq!(teammate_outcome.zone_of(first_wasteland), Zone::Graveyard);
    assert_eq!(teammate_outcome.zone_of(teammate_land), Zone::Graveyard);
    assert_eq!(teammate_outcome.zone_of(opponent_land), Zone::Battlefield);
    assert_eq!(
        teammate_outcome.zone_of(initial_opponent_land),
        Zone::Graveyard
    );
    assert_eq!(teammate_outcome.zone_of(detritivore), Zone::Battlefield);
    assert_eq!(
        teammate_outcome.power_toughness(detritivore),
        (1, 1),
        "a teammate's nonbasic land must not contribute to Detritivore's CDA"
    );

    let opponent_outcome = runner
        .activate(second_wasteland, 1)
        .target_object(opponent_land)
        .resolve();
    assert_eq!(opponent_outcome.zone_of(second_wasteland), Zone::Graveyard);
    assert_eq!(opponent_outcome.zone_of(teammate_land), Zone::Graveyard);
    assert_eq!(opponent_outcome.zone_of(opponent_land), Zone::Graveyard);
    assert_eq!(opponent_outcome.zone_of(detritivore), Zone::Battlefield);
    assert_eq!(
        opponent_outcome.power_toughness(detritivore),
        (2, 2),
        "an opponent's nonbasic land must increase the settled CDA"
    );
}

/// Synthetic grammar fixture: no printed singular qualified zone-count quantity
/// was found; Detritivore is the nearest printed plural witness, retained above.
/// CR 205.4c: Basic Forest is excluded from the nonbasic land count.
/// CR 109.5: "your opponents" is relative to this static's current controller.
/// CR 613.4c: two matching cards modify the base 1/1 creature to 3/3.
#[test]
fn issue_10130_singular_qualified_zone_count_reaches_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_land_to_graveyard(P1, "Wasteland");
    scenario.add_land_to_graveyard(P1, "Command Tower");
    let forest = scenario.add_land_to_graveyard(P1, "Forest").id();
    scenario.add_land_to_graveyard(P0, "Strip Mine");
    let plural = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic Plural Qualified Zone Count",
            1,
            1,
            "This creature gets +1/+1 for each nonbasic land cards in your opponents' graveyards.",
        )
        .id();
    let singular = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic Singular Qualified Zone Count",
            1,
            1,
            "This creature gets +1/+1 for each nonbasic land card in your opponents' graveyards.",
        )
        .id();
    let mut runner = scenario.build();

    // CR 205.4a: the fixture's Basic supertype is explicit on both type records.
    let forest = runner.state_mut().objects.get_mut(&forest).unwrap();
    forest.card_types.supertypes.push(Supertype::Basic);
    forest.base_card_types.supertypes.push(Supertype::Basic);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let pt = |id: ObjectId| {
        let obj = &runner.state().objects[&id];
        (obj.power, obj.toughness)
    };
    // Positive reach guard precedes the singular regression assertion.
    assert_eq!(pt(plural), (Some(3), Some(3)), "plural reach guard");
    assert_eq!(pt(singular), (Some(3), Some(3)), "singular zone count");
    for id in [plural, singular] {
        let obj = &runner.state().objects[&id];
        assert!(!obj.static_definitions.is_empty());
        assert!(!obj
            .abilities
            .iter()
            .any(|ability| matches!(*ability.effect, Effect::Unimplemented { .. })));
    }
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
    assert_eq!(gaps, ["Effect:unknown"]);
}
