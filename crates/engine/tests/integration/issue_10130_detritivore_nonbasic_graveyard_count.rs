//! #10130, slice 1: preserve the supertype qualifier in Detritivore's CDA.
//! Suspend X remains unsupported; the full card must retain exactly that gap.

use engine::game::coverage::card_face_gaps;
use engine::game::effects::token::apply_create_token_after_replacement;
use engine::game::layers::evaluate_layers;
use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::{
    EtbTapState, ProposedEvent, TokenAttackRequest, TokenCharacteristics, TokenHostRequest,
    TokenSpec,
};
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

/// CR 109.2a + CR 111.6: a token is never a graveyard "card", even before
/// CR 704.5d removes it. Check the production zone-move/SBA event boundary,
/// not just the settled snapshot; the 2HG case above covers normal activation.
#[test]
fn issue_10130_detritivore_excludes_everywhere_token_from_graveyard_zone_move_trace() {
    const COMMAND_TOWER: &str =
        "{T}: Add one mana of any color in your commander's color identity.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Keep Detritivore alive throughout normal action/SBA processing.
    let initial_opponent_land = scenario.add_land_to_graveyard(P1, "Command Tower").id();
    let opponent_land = scenario
        .add_land_from_oracle(P1, "Command Tower", COMMAND_TOWER)
        .id();
    let detritivore = scenario
        .add_creature_from_oracle(P0, "Detritivore", 0, 0, DETRITIVORE)
        .id();
    let mut runner = scenario.build();

    // Printed witness: Overlord of the Hauntwoods (DSK 194) says:
    // "Whenever this permanent enters or attacks, create a tapped colorless
    // land token named Everywhere that is every basic land type."
    // https://scryfall.com/card/dsk/194/overlord-of-the-hauntwoods
    // https://scryfall.com/card/tdsk/16/everywhere
    // Transcribe that token's characteristics at the existing production
    // CreateToken setup seam; replacement processing and token realization
    // install its identity, zones and events. Do not use the incomplete
    // Everywhere preset or pretend this exercises Overlord's parser/Impending.
    // The setup-only source id follows the existing token-seam fixtures;
    // this test makes no claim about resolving the printed producing trigger.
    let proposed = ProposedEvent::CreateToken {
        owner: P1,
        spec: Box::new(TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Everywhere".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                core_types: vec![CoreType::Land],
                subtypes: ["Plains", "Island", "Swamp", "Mountain", "Forest"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "Everywhere".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: true,
            enters_attacking: false,
            attack_request: TokenAttackRequest::Unspecified,
            sacrifice_at: None,
            source_id: ObjectId(0),
            controller: P1,
            attach_to: TokenHostRequest::NotRequested,
        }),
        copy: None,
        enter_tapped: EtbTapState::Unspecified,
        count: 1,
        appended: Vec::new(),
        applied: Default::default(),
    };
    let mut creation_events = Vec::new();
    match replace_event(runner.state_mut(), proposed, &mut creation_events) {
        ReplacementResult::Execute(event) => assert!(apply_create_token_after_replacement(
            runner.state_mut(),
            event,
            &mut creation_events,
        )),
        other => panic!("Everywhere token creation must execute, got {other:?}"),
    }
    let everywhere = creation_events
        .iter()
        .find_map(|event| match event {
            GameEvent::TokenCreated {
                object_id, name, ..
            } if name == "Everywhere" => Some(*object_id),
            _ => None,
        })
        .expect("production token realization must emit TokenCreated");
    let token = &runner.state().objects[&everywhere];
    assert!(token.is_token);
    assert!(!token.is_represented_by_a_card());
    assert_eq!(token.owner, P1);
    assert_eq!(token.controller, P1);
    assert_eq!(token.zone, Zone::Battlefield);
    assert!(token.tapped);
    // All five basic land *types* do not confer the Basic *supertype*.
    for types in [&token.card_types, &token.base_card_types] {
        assert_eq!(types.core_types, vec![CoreType::Land]);
        assert!(!types.supertypes.contains(&Supertype::Basic));
        let mut subtypes: Vec<_> = types.subtypes.iter().map(String::as_str).collect();
        subtypes.sort_unstable();
        assert_eq!(
            subtypes,
            ["Forest", "Island", "Mountain", "Plains", "Swamp"]
        );
    }

    // Establish the 1/1 baseline through an ordinary production action.
    runner
        .act(GameAction::PassPriority)
        .expect("normal priority settlement must establish the CDA baseline");
    assert_eq!(
        (
            runner.state().objects[&detritivore].power,
            runner.state().objects[&detritivore].toughness,
        ),
        (Some(1), Some(1)),
    );

    // Observe the lower production zone-delivery -> SBA boundary directly.
    // move_object_for_test only exposes the existing crate-private move_object;
    // it neither evaluates layers nor performs custom cleanup. Ordinary action
    // finalization scans triggers (quietly flushing layers) before SBAs, hiding
    // this publisher's P/T deltas. This is not full spell/activation resolution;
    // the normal Wasteland facade remains exercised by the 2HG case above.
    // Use the moved object itself as the request's attribution anchor, without
    // claiming a printed effect produced this generic effect-driven move.
    let mut token_events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(everywhere, Zone::Graveyard, everywhere),
        &mut token_events,
    ));
    assert_eq!(runner.state().objects[&everywhere].zone, Zone::Graveyard);
    assert!(runner.state().objects[&everywhere].is_token);
    assert!(runner.state().layers_dirty.is_dirty());
    check_state_based_actions(runner.state_mut(), &mut token_events);
    assert!(
        token_events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Graveyard,
                ..
            } if *object_id == everywhere
        )),
        "reach guard: the token must actually enter the opponent's graveyard"
    );
    assert!(
        !runner.state().objects.contains_key(&everywhere),
        "CR 704.5d must remove the token during the production SBA pass"
    );
    assert_eq!(
        runner.state().objects[&initial_opponent_land].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&opponent_land].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&detritivore].zone, Zone::Battlefield);
    let token_action_pt: Vec<_> = token_events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PowerToughnessChanged {
                object_id,
                power,
                toughness,
                ..
            } if *object_id == detritivore => Some((*power, *toughness)),
            _ => None,
        })
        .collect();
    let token_final_pt = (
        runner.state().objects[&detritivore].power,
        runner.state().objects[&detritivore].toughness,
    );
    assert!(
        token_action_pt
            .iter()
            .all(|&(power, toughness)| power <= 1 && toughness <= 1),
        "a token must not cause even a transient CDA increase: {token_action_pt:?}"
    );
    assert_eq!(token_final_pt, (Some(1), Some(1)));

    // Positive control: the same production zone/SBA boundary moves a real
    // opponent nonbasic land card. It must settle at 2/2 AND publish the increase,
    // so the negative token-event assertion is not a silent publisher's vacuity.
    assert!(runner.state().objects[&opponent_land].is_represented_by_a_card());
    let mut card_events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(opponent_land, Zone::Graveyard, opponent_land),
        &mut card_events,
    ));
    assert!(runner.state().layers_dirty.is_dirty());
    check_state_based_actions(runner.state_mut(), &mut card_events);
    assert_eq!(runner.state().objects[&opponent_land].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&detritivore].zone, Zone::Battlefield);
    let card_final_pt = (
        runner.state().objects[&detritivore].power,
        runner.state().objects[&detritivore].toughness,
    );
    assert_eq!(card_final_pt, (Some(2), Some(2)));
    assert!(
        card_events.iter().any(|event| matches!(
            event,
            GameEvent::PowerToughnessChanged {
                object_id,
                power: 2,
                toughness: 2,
                power_delta,
                toughness_delta,
            } if *object_id == detritivore && *power_delta > 0 && *toughness_delta > 0
        )),
        "positive control: a real nonbasic land card must emit the 2/2 increase"
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
