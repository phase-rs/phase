//! Regression coverage for the legacy mass-library ordering prompt captured on
//! turn 15. The fixture predates the typed member snapshot, so the queued owner
//! batches remain tuple-shaped until each successor prompt is published.

use std::io::Read;

use engine::ai_support::legal_actions;
use engine::game::effects::change_zone::resolve_all;
use engine::game::engine::apply_as_current;
use engine::types::ability::{
    ControllerRef, Effect, LibraryPosition, ResolvedAbility, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, MassLibraryOrderBatch, MassLibraryOrderMember, PersistedGameState, StackEntry,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

fn gunzip(gz: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

fn legacy_turn15_persisted() -> PersistedGameState {
    let json = gunzip(include_bytes!(
        "../fixtures/mass_library_order_turn15.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("turn-15 fixture envelope parses");
    serde_json::from_value(envelope["gameState"].clone())
        .expect("turn-15 game state decodes through PersistedGameState")
}

fn assert_select_cards_is_publicly_legal(
    state: &engine::types::game_state::GameState,
    cards: &[ObjectId],
) {
    assert!(
        legal_actions(state).iter().any(|action| {
            matches!(action, GameAction::SelectCards { cards: offered } if offered == cards)
        }),
        "the engine must publish the archived ordering action: {:?}",
        legal_actions(state)
    );
}

fn real_single_owner_mass_library_order_state(
    origin: Zone,
    target: TargetFilter,
) -> (GameState, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    let first = engine::game::zones::create_object(
        &mut state,
        CardId(500),
        PlayerId(0),
        "First Creature".to_string(),
        origin,
    );
    let second = engine::game::zones::create_object(
        &mut state,
        CardId(501),
        PlayerId(0),
        "Second Creature".to_string(),
        origin,
    );
    for card in [first, second] {
        state
            .objects
            .get_mut(&card)
            .expect("newly created card exists")
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
    }
    let ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(origin),
            destination: Zone::Library,
            target,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: Some(LibraryPosition::Bottom),
            random_order: false,
        },
        vec![],
        ObjectId(900),
        PlayerId(0),
    );
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(901),
        source_id: ability.source_id,
        controller: PlayerId(0),
        kind: StackEntryKind::ActivatedAbility {
            source_id: ability.source_id,
            ability: Box::new(ability.clone()),
        },
    });

    resolve_all(&mut state, &ability, &mut Vec::new())
        .expect("production ChangeZoneAll opens a mass ordering prompt");
    let cards = match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            cards,
            mass_library_order,
            ..
        } => {
            assert!(mass_library_order.is_some());
            cards.clone()
        }
        other => panic!("expected mass ordering prompt, got {other:?}"),
    };
    assert_eq!(cards.len(), 2);
    assert!(state.pending_mass_library_order_choice.is_none());
    (state, cards)
}

/// A persisted legacy queue must retain its old tuple provenance on every save
/// until it has promoted each owner batch. The public reducer can then advance
/// both owners back to priority without relaxing ordinary library-zone choices.
#[test]
fn turn15_legacy_mass_order_survives_save_reload_and_both_owners_complete() {
    let persisted = legacy_turn15_persisted();
    let saved = serde_json::to_string(&persisted).expect("legacy state saves");
    assert!(
        saved.contains(r#""remaining_batches":[[1,[161,189,216,211]]]"#),
        "the save must retain the legacy queue that authorizes its successor prompt"
    );
    let mut state = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("saved legacy state decodes")
        .into_game_state()
        .expect("saved legacy state restores through the production chokepoint");

    let current_cards = match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, PlayerId(0));
            assert!(
                mass_library_order.is_none(),
                "the archive must exercise migration"
            );
            cards.clone()
        }
        other => panic!("turn-15 must restore its legacy ordering prompt, got {other:?}"),
    };
    assert_eq!(current_cards, vec![ObjectId(199)]);
    assert_select_cards_is_publicly_legal(&state, &current_cards);
    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: current_cards,
        },
    )
    .expect("current owner can submit the archived ordering");

    let successor_cards = match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, PlayerId(1));
            assert!(
                mass_library_order.is_some(),
                "successor must be promoted to typed state"
            );
            cards.clone()
        }
        other => panic!("the second owner must receive a typed successor prompt, got {other:?}"),
    };
    assert_eq!(
        successor_cards,
        vec![ObjectId(161), ObjectId(189), ObjectId(216), ObjectId(211)]
    );
    assert_select_cards_is_publicly_legal(&state, &successor_cards);
    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: successor_cards,
        },
    )
    .expect("successor owner can submit the promoted ordering");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "completing every legacy batch must finish the resolving spell"
    );
}

/// A fresh typed snapshot is tied to an object incarnation, while a legacy
/// prompt remains tied to the old battlefield origin that its archive proves.
/// Neither stale form may gain the compatibility exception.
#[test]
fn mass_library_order_rejects_stale_typed_incarnation_and_legacy_origin() {
    let mut typed_state = legacy_turn15_persisted()
        .into_game_state()
        .expect("fixture restores");
    let object = typed_state
        .objects
        .get(&ObjectId(199))
        .expect("fixture current member exists");
    let batch = MassLibraryOrderBatch {
        owner: PlayerId(0),
        members: vec![MassLibraryOrderMember {
            identity: ObjectIncarnationRef::from_object(object),
            origin: Zone::Battlefield,
        }],
    };
    let WaitingFor::EffectZoneChoice {
        mass_library_order, ..
    } = &mut typed_state.waiting_for
    else {
        panic!("fixture starts at EffectZoneChoice");
    };
    *mass_library_order = Some(batch);
    let mut typed_reach_guard = typed_state.clone();
    apply_as_current(
        &mut typed_reach_guard,
        GameAction::SelectCards {
            cards: vec![ObjectId(199)],
        },
    )
    .expect("the unmodified typed prompt reaches the production reducer");
    typed_state
        .objects
        .get_mut(&ObjectId(199))
        .expect("fixture current member exists")
        .incarnation += 1;
    assert!(
        apply_as_current(
            &mut typed_state,
            GameAction::SelectCards {
                cards: vec![ObjectId(199)],
            },
        )
        .is_err(),
        "a fresh prompt must reject a replacement incarnation with the same id"
    );

    let mut legacy_state = legacy_turn15_persisted()
        .into_game_state()
        .expect("fixture restores");
    let mut legacy_reach_guard = legacy_state.clone();
    apply_as_current(
        &mut legacy_reach_guard,
        GameAction::SelectCards {
            cards: vec![ObjectId(199)],
        },
    )
    .expect("the unmodified legacy prompt reaches the production reducer");
    legacy_state
        .objects
        .get_mut(&ObjectId(199))
        .expect("fixture current member exists")
        .zone = Zone::Graveyard;
    assert!(
        apply_as_current(
            &mut legacy_state,
            GameAction::SelectCards {
                cards: vec![ObjectId(199)],
            },
        )
        .is_err(),
        "the legacy migration gate must reject a member that left its battlefield origin"
    );
}

/// CR 401.4: A single-owner mass `ChangeZoneAll` creates no continuation
/// carrier, but its real resolution-time prompt must still accept the owner's
/// submitted order and finish at priority.
#[test]
fn single_owner_mass_library_order_completes_through_change_zone_all() {
    let (mut state, cards) = real_single_owner_mass_library_order_state(
        Zone::Battlefield,
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
    );
    let &[first, _second] = cards.as_slice() else {
        panic!("production prompt must contain exactly two cards");
    };

    let nonmatching = engine::game::zones::create_object(
        &mut state,
        CardId(502),
        PlayerId(0),
        "Nonmatching Artifact".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&nonmatching)
        .unwrap()
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Artifact);
    let mut substituted = state.clone();
    let WaitingFor::EffectZoneChoice {
        cards: prompt_cards,
        mass_library_order,
        ..
    } = &mut substituted.waiting_for
    else {
        panic!("the production prompt must remain an EffectZoneChoice");
    };
    *prompt_cards = vec![first, nonmatching];
    *mass_library_order = None;
    assert!(
        apply_as_current(
            &mut substituted,
            GameAction::SelectCards {
                cards: vec![first, nonmatching],
            },
        )
        .is_err(),
        "a same-owner battlefield card outside the resolving target filter must not substitute"
    );

    let WaitingFor::EffectZoneChoice {
        mass_library_order, ..
    } = &mut state.waiting_for
    else {
        panic!("the production prompt must remain an EffectZoneChoice");
    };
    *mass_library_order = None;

    apply_as_current(&mut state, GameAction::SelectCards { cards })
        .expect("production mass ordering selection is accepted");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn legacy_marker_removed_real_continuation_preserves_hand_and_graveyard_origins() {
    for origin in [Zone::Hand, Zone::Graveyard] {
        let (mut state, cards) = real_single_owner_mass_library_order_state(
            origin,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
        let WaitingFor::EffectZoneChoice {
            mass_library_order, ..
        } = &mut state.waiting_for
        else {
            panic!("the production prompt must remain an EffectZoneChoice");
        };
        *mass_library_order = None;

        apply_as_current(&mut state, GameAction::SelectCards { cards })
            .expect("the legacy continuation accepts its producer's origin");
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}

#[test]
fn legacy_marker_removed_hand_controller_continuation_reaches_priority() {
    let (mut state, cards) =
        real_single_owner_mass_library_order_state(Zone::Hand, TargetFilter::Controller);
    let WaitingFor::EffectZoneChoice {
        mass_library_order, ..
    } = &mut state.waiting_for
    else {
        panic!("the production prompt must remain an EffectZoneChoice");
    };
    *mass_library_order = None;

    apply_as_current(&mut state, GameAction::SelectCards { cards })
        .expect("the legacy continuation preserves ChangeZoneAll controller scope");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// The compatibility gate proves a very specific archived producer. A prompt
/// that merely looks like it orders multiple battlefield cards stays subject to
/// normal advertised-zone validation.
#[test]
fn ordinary_unmarked_battlefield_library_prompt_is_rejected() {
    let mut state = legacy_turn15_persisted()
        .into_game_state()
        .expect("fixture restores");
    state.pending_mass_library_order_choice = None;
    state.resolving_stack_entry = None;

    assert!(
        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![ObjectId(199)],
            },
        )
        .is_err(),
        "an unmarked battlefield prompt must not receive the mass-order exception"
    );
}
