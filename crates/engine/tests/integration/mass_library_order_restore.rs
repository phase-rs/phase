//! Regression coverage for the legacy mass-library ordering prompt captured on
//! turn 15. The fixture predates the typed member snapshot, so the queued owner
//! batches remain tuple-shaped until each successor prompt is published.

use std::io::Read;

use engine::ai_support::legal_actions;
use engine::game::engine::apply_as_current;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    MassLibraryOrderBatch, MassLibraryOrderMember, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

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

/// Before the typed queue existed, a single owner with multiple cards had no
/// continuation carrier. Its archived prompt remains admissible only while the
/// resolving `ChangeZoneAll` producer and every current owner/member agree.
#[test]
fn legacy_single_owner_mass_order_without_pending_carrier_is_legal() {
    std::thread::Builder::new()
        .name("legacy-mass-order-admission".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let mut state = legacy_turn15_persisted()
                .into_game_state()
                .expect("fixture restores");
            let cards = vec![ObjectId(161), ObjectId(189), ObjectId(216), ObjectId(211)];

            state.pending_mass_library_order_choice = None;
            state.priority_player = PlayerId(1);
            let WaitingFor::EffectZoneChoice {
                player,
                cards: offered_cards,
                count,
                min_count,
                ..
            } = &mut state.waiting_for
            else {
                panic!("fixture starts at EffectZoneChoice");
            };
            *player = PlayerId(1);
            *offered_cards = cards.clone();
            *count = cards.len();
            *min_count = cards.len();

            assert_select_cards_is_publicly_legal(&state, &cards);
        })
        .expect("large-stack admission test thread starts")
        .join()
        .expect("large-stack admission test thread completes");
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
