use crate::support::shared_card_db;
use engine::ai_support::legal_actions;
use engine::game::casting::{
    can_cast_modal_face_now, can_cast_object_now, spell_objects_available_to_cast,
};
use engine::game::engine::EngineError;
use engine::game::filter::spell_record_matches_filter;
use engine::game::game_object::RoomDoor;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    ControllerRef, FilterProp, GameRestriction, PossessionAxis, ProhibitedActivity,
    RestrictionExpiry, RestrictionPlayerScope, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{CastingVariant, SpellCastRecord, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn advance_to_main(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..160 {
        if runner.state().active_player == player && runner.state().phase == Phase::PreCombatMain {
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            ref other => panic!("unexpected turn prompt {other:?}"),
        };
        runner.act(action).unwrap();
    }
    panic!("did not reach {player:?}'s main phase");
}

fn resolve_weight_room_manifest(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    let WaitingFor::ManifestDreadChoice {
        player: P1, cards, ..
    } = &runner.state().waiting_for
    else {
        panic!("Weight Room should ask P1 to manifest a card");
    };
    assert_eq!(cards.len(), 2);
    let chosen = cards[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&chosen].face_down);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

#[test]
fn departed_mage_token_restricts_only_the_owner_until_the_installers_next_turn() {
    let db = shared_card_db().expect("integration card fixture");
    let controller = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1, controller] {
        scenario.with_library_top(player, &["Island"; 4]);
        for _ in 0..4 {
            scenario.add_basic_land(player, ManaColor::Blue);
        }
    }
    let mage = scenario.add_real_card(P0, "Reflector Mage", Zone::Battlefield, db);
    let copy = scenario.add_real_card(P0, "Cackling Counterpart", Zone::Hand, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    let stolen = scenario
        .add_creature(P1, "Spectral Sailor", 1, 1)
        .controlled_by(controller)
        .id();
    let same = scenario.add_real_card(P1, "Spectral Sailor", Zone::Hand, db);
    let different = scenario.add_real_card(P1, "Opt", Zone::Hand, db);
    let controller_copy = scenario.add_real_card(controller, "Spectral Sailor", Zone::Hand, db);
    let mut runner = scenario.build();
    assert!(can_cast_object_now(runner.state(), P1, same));
    let outcome = runner.cast(copy).target_objects(&[mage, stolen]).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().players[P1.0 as usize].hand.contains(&stolen));
    let [GameRestriction::ProhibitActivity {
        source: token,
        affected_players: RestrictionPlayerScope::SpecificPlayer(owner),
        expiry: RestrictionExpiry::UntilPlayerNextTurn { player },
        activity:
            ProhibitedActivity::CastSpells {
                spell_filter: Some(TargetFilter::Typed(filter)),
            },
    }] = runner.state().restrictions.as_slice()
    else {
        panic!("expected one captured restriction")
    };
    assert_eq!((*owner, *player), (P1, P0));
    assert_eq!(
        filter,
        &TypedFilter::default().properties(vec![FilterProp::Named {
            name: "Spectral Sailor".into()
        }])
    );
    let token = *token;
    assert!(runner.state().objects[&token].is_token);
    assert!(!can_cast_object_now(runner.state(), P1, same));
    runner.cast(unsummon).target_object(token).resolve();
    assert!(!runner.state().objects.contains_key(&token));
    advance_to_main(&mut runner, P1);
    assert!(!can_cast_object_now(runner.state(), P1, same));
    assert!(can_cast_object_now(runner.state(), P1, different));
    assert!(can_cast_object_now(
        runner.state(),
        controller,
        controller_copy
    ));
    // CR 601.3: the cast action itself rejects the matching spell.
    assert!(matches!(
        runner.cast(same).try_resolve(),
        Err(EngineError::ActionNotAllowed(reason)) if reason.contains("temporary effect")
    ));
    advance_to_main(&mut runner, P0);
    assert!(runner.state().restrictions.is_empty());
    assert!(can_cast_object_now(runner.state(), P1, same));
}

#[test]
fn unresolved_owner_and_source_relative_name_filters_restrict_no_player() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opt = scenario.add_real_card(P0, "Opt", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();
    let absent = ObjectId(9999);
    assert!(!runner.state().objects.contains_key(&absent));
    let named = TypedFilter::default().properties(vec![FilterProp::Named { name: "Opt".into() }]);
    assert!(can_cast_object_now(runner.state(), P0, opt));
    for (scope, filter, can_cast) in [
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.clone(),
            false,
        ),
        (
            RestrictionPlayerScope::ParentObjectTarget {
                possession: PossessionAxis::Owner,
            },
            named.clone(),
            true,
        ),
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.clone().controller(ControllerRef::You),
            true,
        ),
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.clone().with_type(TypeFilter::Instant),
            true,
        ),
        (
            RestrictionPlayerScope::SpecificPlayer(P0),
            named.properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::Named { name: "Opt".into() },
                    FilterProp::InZone { zone: Zone::Hand },
                ],
            }]),
            true,
        ),
    ] {
        runner.state_mut().restrictions = vec![GameRestriction::ProhibitActivity {
            source: absent,
            affected_players: scope,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::CastSpells {
                spell_filter: Some(TargetFilter::Typed(filter)),
            },
        }];
        assert_eq!(can_cast_object_now(runner.state(), P0, opt), can_cast);
    }
}

#[test]
fn bounced_face_down_creature_shares_no_name_with_its_printed_face_or_another_face_down_cast() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island"; 3]);
        for _ in 0..3 {
            scenario.add_basic_land(player, ManaColor::Blue);
        }
    }
    scenario.add_basic_land(P1, ManaColor::White);
    let morph = scenario.add_real_card(P0, "Willbender", Zone::Hand, db);
    let mage = scenario.add_real_card(P1, "Reflector Mage", Zone::Hand, db);
    let mut runner = scenario.build();
    runner
        .cast(morph)
        .casting_variant(CastingVariant::FaceDown)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert!(runner.state().objects[&morph].face_down);
    advance_to_main(&mut runner, P1);
    let outcome = runner.cast(mage).target_object(morph).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&morph].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&morph].name, "Willbender");
    advance_to_main(&mut runner, P0);
    assert!(matches!(runner.state().restrictions.as_slice(), [
        GameRestriction::ProhibitActivity {
            affected_players: RestrictionPlayerScope::SpecificPlayer(P0),
            activity: ProhibitedActivity::CastSpells { spell_filter: Some(TargetFilter::Typed(filter)) },
            ..
        }
    ] if matches!(filter.properties.as_slice(), [FilterProp::AnyOf { props }] if props.is_empty())));
    // CR 201.2a + CR 708.4: the bounced creature had no name to share with
    // either the printed card or the face-down spell.
    assert!(can_cast_object_now(runner.state(), P0, morph));
    runner
        .cast(morph)
        .casting_variant(CastingVariant::FaceDown)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert!(runner.state().objects[&morph].face_down);
    assert_eq!(runner.state().objects[&morph].zone, Zone::Battlefield);
}

#[test]
fn the_restricted_owner_can_play_the_bounced_dryad_arbor() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Forest"; 3]);
    for mana in [ManaColor::Blue, ManaColor::White, ManaColor::Blue] {
        scenario.add_basic_land(P0, mana);
    }
    let mage = scenario.add_real_card(P0, "Reflector Mage", Zone::Hand, db);
    let arbor = scenario.add_real_card(P1, "Dryad Arbor", Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner.cast(mage).target_object(arbor).resolve();
    assert_eq!(runner.state().objects[&arbor].zone, Zone::Hand);
    advance_to_main(&mut runner, P1);
    assert!(matches!(runner.state().restrictions.as_slice(), [
        GameRestriction::ProhibitActivity {
            affected_players: RestrictionPlayerScope::SpecificPlayer(P1),
            activity: ProhibitedActivity::CastSpells { spell_filter: Some(TargetFilter::Typed(filter)) },
            ..
        }
    ] if matches!(filter.properties.as_slice(), [FilterProp::Named { name }] if name == "Dryad Arbor")));

    // CR 305.1 + CR 116.2a: the restricted player can play a land with the
    // captured name during the prohibition.
    let play = GameAction::PlayLand {
        object_id: arbor,
        card_id: runner.state().objects[&arbor].card_id,
    };
    assert!(legal_actions(runner.state()).contains(&play));
    runner.act(play).unwrap();
    assert_eq!(runner.state().objects[&arbor].zone, Zone::Battlefield);
}

#[test]
fn a_face_up_morph_is_prohibited_but_its_face_down_cast_is_offered() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"; 3]);
    for mana in [ManaColor::Blue, ManaColor::White, ManaColor::Blue] {
        scenario.add_basic_land(P0, mana);
    }
    for _ in 0..2 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let mage = scenario.add_real_card(P0, "Reflector Mage", Zone::Hand, db);
    let morph = scenario.add_real_card(P1, "Willbender", Zone::Battlefield, db);
    let land = scenario.add_real_card(P1, "Island", Zone::Hand, db);
    let mut runner = scenario.build();

    runner.cast(mage).target_object(morph).resolve();
    assert_eq!(runner.state().objects[&morph].zone, Zone::Hand);
    advance_to_main(&mut runner, P1);
    // CR 601.3 + CR 702.37c: two mana would pay the printed cost. The only
    // permitted proposal costs three, so attempting the cast is rejected.
    assert!(matches!(
        runner.cast(morph).try_resolve(),
        Err(EngineError::ActionNotAllowed(reason)) if reason.contains("temporary effect")
    ));
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id: runner.state().objects[&land].card_id,
        })
        .unwrap();
    // CR 601.3a + CR 708.4: the face-down proposal is nameless.
    assert!(spell_objects_available_to_cast(runner.state(), P1).contains(&morph));
    assert!(legal_actions(runner.state()).iter().any(|action| {
        matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == morph)
    }));
    let outcome = runner
        .cast(morph)
        .casting_variant(CastingVariant::FaceDown)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    outcome.assert_zone(&[morph], Zone::Battlefield);
    assert!(outcome.state().objects[&morph].face_down);
}

#[test]
fn the_room_restriction_matches_only_the_names_of_unlocked_doors() {
    let db = shared_card_db().expect("integration card fixture");
    for unlock_right in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Island"; 3]);
        scenario.with_library_top(P1, &["Forest"; 8]);
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::Blue);
        }
        for _ in 0..9 {
            scenario.add_basic_land(P1, ManaColor::Green);
        }
        let mage = scenario.add_real_card(P0, "Reflector Mage", Zone::Battlefield, db);
        let counterpart = scenario.add_real_card(P0, "Cackling Counterpart", Zone::Hand, db);
        let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
        scenario.add_real_card(P0, "Opalescence", Zone::Battlefield, db);
        let room = scenario.add_real_card(P1, "Moldering Gym", Zone::Hand, db);
        let other = scenario.add_real_card(P1, "Moldering Gym", Zone::Hand, db);
        let mut runner = scenario.build();

        advance_to_main(&mut runner, P1);
        assert!(can_cast_modal_face_now(runner.state(), P1, other, false));
        assert!(can_cast_modal_face_now(runner.state(), P1, other, true));
        runner
            .cast(room)
            .modal_back_face(false)
            .search_first_legal()
            .resolve();
        if unlock_right {
            runner
                .act(GameAction::UnlockRoomDoor {
                    object_id: room,
                    door: RoomDoor::Right,
                })
                .unwrap();
            resolve_weight_room_manifest(&mut runner);
        }
        let departure_name = if unlock_right {
            "Moldering Gym // Weight Room"
        } else {
            "Moldering Gym"
        };
        assert_eq!(runner.state().objects[&room].name, departure_name);

        advance_to_main(&mut runner, P0);
        let outcome = runner
            .cast(counterpart)
            .target_objects(&[mage, room])
            .resolve();
        outcome.assert_zone(&[room], Zone::Hand);
        let departure = &runner.state().lki_cache[&room];
        assert_eq!(departure.name, departure_name);
        assert!(departure.subtypes.iter().any(|subtype| subtype == "Room"));
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        let [GameRestriction::ProhibitActivity {
            source: token,
            activity:
                ProhibitedActivity::CastSpells {
                    spell_filter: Some(filter),
                },
            ..
        }] = runner.state().restrictions.as_slice()
        else {
            panic!("expected a captured cast prohibition")
        };
        // CR 201.2a + CR 608.2h + CR 709.5: use the Room's effective names
        // immediately before it moved, including only its unlocked doors.
        for (name, expected) in [
            ("Moldering Gym", true),
            ("Weight Room", unlock_right),
            ("Moldering Gym // Weight Room", false),
        ] {
            let record = SpellCastRecord {
                name: name.into(),
                ..Default::default()
            };
            assert_eq!(
                spell_record_matches_filter(&record, filter, P1, &[]),
                expected
            );
        }
        let token = *token;
        runner.cast(unsummon).target_object(token).resolve();
        assert!(!runner.state().objects.contains_key(&token));

        advance_to_main(&mut runner, P1);
        assert!(!can_cast_modal_face_now(runner.state(), P1, other, false));
        assert_eq!(
            can_cast_modal_face_now(runner.state(), P1, other, true),
            !unlock_right
        );
        assert_eq!(
            spell_objects_available_to_cast(runner.state(), P1).contains(&other),
            !unlock_right
        );
        if !unlock_right {
            runner
                .cast(other)
                .modal_back_face(true)
                .resolve()
                .assert_zone(&[other], Zone::Battlefield);
            resolve_weight_room_manifest(&mut runner);
        }
    }
}

#[test]
fn room_restriction_keeps_both_names_after_oko_removes_room_subtype() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 3]);
    scenario.with_library_top(P1, &["Forest"; 8]);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    scenario.add_basic_land(P0, ManaColor::White);
    for _ in 0..9 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let mage = scenario.add_real_card(P0, "Reflector Mage", Zone::Hand, db);
    scenario.add_real_card(P1, "Opalescence", Zone::Battlefield, db);
    let oko = scenario.add_real_card(P1, "Oko, Thief of Crowns", Zone::Battlefield, db);
    let room = scenario.add_real_card(P1, "Moldering Gym", Zone::Hand, db);
    let other = scenario.add_real_card(P1, "Moldering Gym", Zone::Hand, db);
    let mut runner = scenario.build();

    advance_to_main(&mut runner, P1);
    runner
        .cast(room)
        .modal_back_face(false)
        .search_first_legal()
        .resolve();
    runner
        .act(GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Right,
        })
        .unwrap();
    resolve_weight_room_manifest(&mut runner);
    assert_eq!(
        runner.state().objects[&room].name,
        "Moldering Gym // Weight Room"
    );

    // Oko's real [+1] ability changes the Room's types and subtypes before the
    // Reflector Mage trigger captures its last-known information.
    runner.activate(oko, 1).target_object(room).resolve();
    let changed_room = &runner.state().objects[&room];
    assert!(!changed_room
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Room"));
    assert_eq!(changed_room.name, "Moldering Gym // Weight Room");

    advance_to_main(&mut runner, P0);
    runner.cast(mage).target_object(room).resolve();
    let departure = &runner.state().lki_cache[&room];
    assert_eq!(departure.name, "Moldering Gym // Weight Room");

    let [GameRestriction::ProhibitActivity {
        source: _,
        activity: ProhibitedActivity::CastSpells {
            spell_filter: Some(filter),
        },
        ..
    }] = runner.state().restrictions.as_slice()
    else {
        panic!("expected a captured cast prohibition")
    };
    for name in ["Moldering Gym", "Weight Room"] {
        let record = SpellCastRecord {
            name: name.into(),
            ..Default::default()
        };
        assert!(spell_record_matches_filter(&record, filter, P1, &[]));
    }
    let joined = SpellCastRecord {
        name: "Moldering Gym // Weight Room".into(),
        ..Default::default()
    };
    assert!(!spell_record_matches_filter(&joined, filter, P1, &[]));

    advance_to_main(&mut runner, P1);
    assert!(!can_cast_modal_face_now(runner.state(), P1, other, false));
    assert!(!can_cast_modal_face_now(runner.state(), P1, other, true));
}
