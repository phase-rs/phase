use super::*;
use crate::game::zones::create_object;
use crate::types::card_type::CoreType;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::statics::{CrewAction, CrewContributionKind, StaticMode};
use crate::types::zones::Zone;
use crate::types::{StaticDefinition, TargetFilter};

fn setup_game_at_main_phase() -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

/// Set up a Vehicle (Crew 3) and creatures for crew tests.
fn setup_crew_scenario() -> (GameState, ObjectId, ObjectId, ObjectId) {
    let mut state = setup_game_at_main_phase();

    // Create a Vehicle with Crew 3 and 6/5 P/T
    let vehicle_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Test Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&vehicle_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.card_types.subtypes.push("Vehicle".to_string());
        obj.keywords.push(crate::types::keywords::Keyword::Crew {
            power: 3,
            once_per_turn: None,
        });
        obj.base_power = Some(6);
        obj.base_toughness = Some(5);
        obj.power = Some(6);
        obj.toughness = Some(5);
    }

    // Create a 3/3 creature
    let creature_a = create_object(
        &mut state,
        CardId(201),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_a).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
    }

    // Create a 2/2 creature
    let creature_b = create_object(
        &mut state,
        CardId(202),
        PlayerId(0),
        "Squire".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_b).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    (state, vehicle_id, creature_a, creature_b)
}

#[test]
fn test_crew_activation_enters_crew_vehicle_state() {
    let (mut state, vehicle_id, creature_a, creature_b) = setup_crew_scenario();

    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::CrewVehicle {
            player,
            vehicle_id: vid,
            crew_power,
            eligible_creatures,
        } => {
            assert_eq!(player, PlayerId(0));
            assert_eq!(vid, vehicle_id);
            assert_eq!(crew_power, 3);
            assert!(eligible_creatures.contains(&creature_a));
            assert!(eligible_creatures.contains(&creature_b));
        }
        other => panic!("Expected CrewVehicle, got {:?}", other),
    }
}

#[test]
fn test_crew_resolution_single_creature_meets_threshold() {
    let (mut state, vehicle_id, creature_a, _creature_b) = setup_crew_scenario();

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    // Announcement: cost paid, keyword-action stack entry pushed.
    let announce = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();
    assert!(state.objects.get(&creature_a).unwrap().tapped);
    assert_eq!(state.stack.len(), 1, "Crew announcement pushes stack entry");
    assert!(
        !announce
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::VehicleCrewed { .. })),
        "VehicleCrewed event must not fire until stack resolution"
    );

    // Pass priority; stack resolves → Vehicle becomes a creature, event fires.
    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(state.stack.is_empty(), "stack empty after resolution");
    assert_eq!(
        state.objects.get(&vehicle_id).unwrap().zone,
        Zone::Battlefield
    );
    assert!(resolve.events.iter().any(|e| matches!(
        e,
        GameEvent::VehicleCrewed {
            vehicle_id: vid,
            creatures,
        } if *vid == vehicle_id && creatures == &[creature_a]
    )));
}

#[test]
fn test_crew_resolution_multiple_creatures_sum_power() {
    let (mut state, vehicle_id, creature_a, creature_b) = setup_crew_scenario();

    // Make creature_a only power 2 so both are needed
    state.objects.get_mut(&creature_a).unwrap().power = Some(2);
    state.objects.get_mut(&creature_a).unwrap().base_power = Some(2);

    // Activate crew
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    // Resolve with both creatures (2 + 2 = 4 >= 3)
    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a, creature_b],
        },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.objects.get(&creature_a).unwrap().tapped);
    assert!(state.objects.get(&creature_b).unwrap().tapped);
}

#[test]
fn test_crew_excludes_creature_with_cant_crew() {
    let (mut state, vehicle_id, creature_a, creature_b) = setup_crew_scenario();
    state
        .objects
        .get_mut(&creature_b)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::CantCrew));
    assert!(!crate::game::static_abilities::object_has_cant_crew(
        &state, creature_a
    ));
    assert!(crate::game::static_abilities::object_has_cant_crew(
        &state, creature_b
    ));

    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::CrewVehicle {
            eligible_creatures, ..
        } => {
            assert!(eligible_creatures.contains(&creature_a));
            assert!(!eligible_creatures.contains(&creature_b));
        }
        other => panic!("Expected CrewVehicle, got {:?}", other),
    }

    let err = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_b],
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::InvalidAction(_)));
}

#[test]
fn test_crew_fails_insufficient_power() {
    let (mut state, vehicle_id, _creature_a, creature_b) = setup_crew_scenario();

    // Activate crew
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    // creature_b has power 2, threshold is 3
    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_b],
        },
    );

    assert!(result.is_err());
}

/// CR 702.122a: a creature with "crews Vehicles as though its power were N
/// greater" (Reckoner Bankbuster) contributes its modified power, letting an
/// otherwise-insufficient creature pay the crew cost alone.
#[test]
fn crew_contribution_power_delta_lets_low_power_creature_crew() {
    let (mut state, vehicle_id, _creature_a, creature_b) = setup_crew_scenario();
    // creature_b is 2/2; the Vehicle needs Crew 3, so it cannot crew alone
    // (see `test_crew_fails_insufficient_power`). Grant it the +2 modifier.
    {
        let obj = state.objects.get_mut(&creature_b).unwrap();
        obj.static_definitions.push(
            StaticDefinition::new(StaticMode::CrewContribution {
                kind: CrewContributionKind::PowerDelta { delta: 2 },
                actions: vec![CrewAction::Crew],
            })
            .affected(TargetFilter::SelfRef),
        );
    }

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();
    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_b],
        },
    );
    assert!(
        result.is_ok(),
        "power 2 + delta 2 = 4 should satisfy Crew 3: {result:?}"
    );
}

/// CR 702.122a: regression — the legal-action enumerator must measure crew
/// contribution through `object_crew_power_contribution`, exactly like the
/// activation gate and announcement validator. A Pilot-style creature whose
/// raw power is below the crew cost but whose adjusted power meets it must
/// still produce a `CrewVehicle` legal action; otherwise the controller is
/// offered an empty action set in the `CrewVehicle` state and hangs.
/// (Reproduces the reported Deathless Pilot / Hulldrifter Crew-3 stall.)
#[test]
fn crew_vehicle_legal_actions_account_for_power_delta_contribution() {
    let (mut state, vehicle_id, creature_a, creature_b) = setup_crew_scenario();
    // Tap the 3/3 so the only eligible crewer is the 2/2 Pilot, mirroring
    // the report where the sole eligible creature is sub-threshold by raw
    // power but meets Crew 3 via "+2 greater".
    state.objects.get_mut(&creature_a).unwrap().tapped = true;
    {
        let obj = state.objects.get_mut(&creature_b).unwrap();
        obj.static_definitions.push(
            StaticDefinition::new(StaticMode::CrewContribution {
                kind: CrewContributionKind::PowerDelta { delta: 2 },
                actions: vec![CrewAction::Crew],
            })
            .affected(TargetFilter::SelfRef),
        );
    }

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    let actions = crate::ai_support::legal_actions(&state);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CrewVehicle { creature_ids, .. } if creature_ids == &vec![creature_b]
        )),
        "Crew-3 with only a power-2 Pilot (+2 delta) must offer a crew action, got {actions:?}"
    );
}

/// CR 702.122a: "using its toughness rather than its power" (Giant Ox)
/// substitutes toughness for power, and the modifier applies only to the
/// named keyword actions (crew-only here, not saddle).
#[test]
fn crew_contribution_toughness_substitution_and_action_scope() {
    let (mut state, _vehicle_id, _creature_a, creature_b) = setup_crew_scenario();
    {
        let obj = state.objects.get_mut(&creature_b).unwrap();
        obj.power = Some(0);
        obj.toughness = Some(4);
        obj.static_definitions.push(
            StaticDefinition::new(StaticMode::CrewContribution {
                kind: CrewContributionKind::ToughnessInsteadOfPower,
                actions: vec![CrewAction::Crew],
            })
            .affected(TargetFilter::SelfRef),
        );
    }
    // Crew: contributes toughness (4) instead of power (0).
    assert_eq!(
        crate::game::static_abilities::object_crew_power_contribution(
            &state,
            creature_b,
            CrewAction::Crew
        ),
        4
    );
    // Saddle: the modifier is crew-only, so the base power (0) is contributed.
    assert_eq!(
        crate::game::static_abilities::object_crew_power_contribution(
            &state,
            creature_b,
            CrewAction::Saddle
        ),
        0
    );
}

#[test]
fn test_crew_succeeds_at_instant_speed() {
    // CR 702.122a: Crew has no "Activate only as a sorcery" restriction —
    // unlike Equip (CR 702.6a) and Saddle (CR 702.171a).
    let (mut state, vehicle_id, creature_a, _creature_b) = setup_crew_scenario();
    state.phase = Phase::BeginCombat;

    // Activation should succeed during combat
    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::CrewVehicle { .. }));

    // Resolution should also succeed
    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.objects.get(&creature_a).unwrap().tapped);
}

#[test]
fn test_crew_fails_not_a_vehicle() {
    let mut state = setup_game_at_main_phase();

    // Create a non-Vehicle artifact
    let artifact_id = create_object(
        &mut state,
        CardId(300),
        PlayerId(0),
        "Not A Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&artifact_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.keywords.push(crate::types::keywords::Keyword::Crew {
            power: 1,
            once_per_turn: None,
        });
    }

    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id: artifact_id,
            creature_ids: vec![],
        },
    );

    assert!(result.is_err());
}

#[test]
fn test_crew_vehicle_excludes_itself_from_eligible() {
    let (mut state, vehicle_id, _creature_a, _creature_b) = setup_crew_scenario();

    // Make the Vehicle also a creature (e.g., from a prior crew)
    state
        .objects
        .get_mut(&vehicle_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let result = apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id,
            creature_ids: vec![],
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::CrewVehicle {
            eligible_creatures, ..
        } => {
            // Vehicle should NOT be in eligible creatures even though it's a creature
            assert!(!eligible_creatures.contains(&vehicle_id));
        }
        other => panic!("Expected CrewVehicle, got {:?}", other),
    }
}

// CR 702.122a + CR 702.122b: A Vehicle that has become an artifact creature
// via Crew may contribute to crewing another Vehicle.
#[test]
fn test_crewed_vehicle_may_crew_another_vehicle() {
    let (mut state, vehicle_a, creature_a, _creature_b) = setup_crew_scenario();

    let vehicle_b = create_object(
        &mut state,
        CardId(204),
        PlayerId(0),
        "Second Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&vehicle_b).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Vehicle".to_string());
        obj.keywords.push(crate::types::keywords::Keyword::Crew {
            power: 3,
            once_per_turn: None,
        });
        obj.power = Some(6);
        obj.toughness = Some(5);
    }

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id: vehicle_a,
            creature_ids: vec![],
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id: vehicle_a,
            creature_ids: vec![creature_a],
        },
    )
    .unwrap();
    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        state
            .objects
            .get(&vehicle_a)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "Vehicle A should be an artifact creature after crew resolves"
    );

    apply_as_current(
        &mut state,
        GameAction::CrewVehicle {
            vehicle_id: vehicle_b,
            creature_ids: vec![],
        },
    )
    .unwrap();

    match &state.waiting_for {
        WaitingFor::CrewVehicle {
            eligible_creatures, ..
        } => assert!(
            eligible_creatures.contains(&vehicle_a),
            "crewed Vehicle A should be eligible to crew Vehicle B"
        ),
        other => panic!("Expected CrewVehicle, got {:?}", other),
    }
}

/// Build a Vehicle (Artifact + "Vehicle" subtype) with a printed
/// `Crew { power }` in BOTH `base_keywords` and `keywords`, so the printed
/// keyword survives the `obj.keywords = obj.base_keywords.clone()` reset
/// at the top of every `evaluate_layers` pass.
fn make_printed_crew_vehicle(
    state: &mut GameState,
    card: CardId,
    controller: PlayerId,
    crew_power: u32,
) -> ObjectId {
    let id = create_object(
        state,
        card,
        controller,
        "Printed Vehicle".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".to_string());
    let crew = crate::types::keywords::Keyword::Crew {
        power: crew_power,
        once_per_turn: None,
    };
    obj.base_keywords.push(crew.clone());
    obj.keywords.push(crew);
    obj.base_power = Some(6);
    obj.base_toughness = Some(5);
    obj.power = Some(6);
    obj.toughness = Some(5);
    id
}

/// Attach a "Vehicles you control have crew N" continuous static (the
/// Kotori, Pilot Prodigy class) to `source`, scoped to Vehicles controlled
/// by `source`'s controller.
fn attach_crew_grant_static(state: &mut GameState, source: ObjectId, granted_power: u32) {
    use crate::types::ability::{ContinuousModification, ControllerRef, TargetFilter, TypedFilter};
    let def = StaticDefinition::continuous()
        .affected(TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::You)
                .subtype("Vehicle".to_string()),
        ))
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: crate::types::keywords::Keyword::Crew {
                power: granted_power,
                once_per_turn: None,
            },
        }]);
    state
        .objects
        .get_mut(&source)
        .unwrap()
        .static_definitions
        .push(def);
}

fn crew_powers(state: &GameState, vehicle: ObjectId) -> Vec<u32> {
    state.objects[&vehicle]
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            crate::types::keywords::Keyword::Crew { power, .. } => Some(*power),
            _ => None,
        })
        .collect()
}

/// Issue #2342 — Kotori, Pilot Prodigy: "Vehicles you control have crew 2."
/// A granted single-authoritative-value Crew must REPLACE the printed Crew
/// rather than coexist with it, so `handle_crew_activation`'s `find_map`
/// reads the granted value. Before the CR 613.7 override branch in
/// `apply_keyword_modification`, the printed `Crew { power: 3 }` and granted
/// `Crew { power: 2 }` would both survive (PartialEq sees them as distinct),
/// leaving two Crew entries and letting the stale printed `3` win the read.
#[test]
fn granted_crew_value_overrides_printed_value() {
    let mut state = setup_game_at_main_phase();
    let vehicle = make_printed_crew_vehicle(&mut state, CardId(300), PlayerId(0), 3);
    let kotori = create_object(
        &mut state,
        CardId(301),
        PlayerId(0),
        "Kotori".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&kotori).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
    }
    attach_crew_grant_static(&mut state, kotori, 2);

    crate::game::layers::evaluate_layers(&mut state);

    // Exactly one Crew entry, carrying the granted value (2), not the
    // printed value (3) — the printed duplicate was removed, not appended.
    assert_eq!(
        crew_powers(&state, vehicle),
        vec![2],
        "granted crew 2 must replace printed crew 3, leaving a single Crew entry"
    );
}

/// Negative control: with no granting static in play, the printed Crew
/// value is left untouched by the override branch. Proves the fix does not
/// regress the default (single printed keyword) case.
#[test]
fn printed_crew_value_unchanged_without_granting_static() {
    let mut state = setup_game_at_main_phase();
    let vehicle = make_printed_crew_vehicle(&mut state, CardId(310), PlayerId(0), 3);

    crate::game::layers::evaluate_layers(&mut state);

    assert_eq!(
        crew_powers(&state, vehicle),
        vec![3],
        "without a granting static the printed crew 3 must be preserved"
    );
}

/// Filter-scope exclusion: the "Vehicles you control" static grant must
/// compose with the existing `TargetFilter`/`ControllerRef` scoping — an
/// opponent-controlled Vehicle is outside the `controller=You` scope and
/// must keep its printed crew value, proving the override branch does not
/// blindly rewrite every Crew entry engine-wide.
#[test]
fn granted_crew_does_not_override_opponents_vehicle() {
    let mut state = setup_game_at_main_phase();
    // Opponent's Vehicle with printed Crew 4.
    let opp_vehicle = make_printed_crew_vehicle(&mut state, CardId(320), PlayerId(1), 4);
    // Granting static controlled by PlayerId(0) — "you control" excludes
    // the opponent's Vehicle.
    let kotori = create_object(
        &mut state,
        CardId(321),
        PlayerId(0),
        "Kotori".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&kotori).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
    }
    attach_crew_grant_static(&mut state, kotori, 2);

    crate::game::layers::evaluate_layers(&mut state);

    assert_eq!(
        crew_powers(&state, opp_vehicle),
        vec![4],
        "opponent's Vehicle is outside the 'you control' scope and must keep printed crew 4"
    );
}
