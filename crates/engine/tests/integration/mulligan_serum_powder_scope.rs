//! CR 103.5b: Serum Powder reads "Any time you could mulligan and this card is
//! in your hand, you may exile all the cards from your hand, then draw that
//! many cards." The `UseSerumPowder` action names an object in one seat's hand,
//! so only a list built for that seat carries it.

use engine::ai_support::{
    build_decision_context_for_semantic_owner, candidate_actions, candidate_actions_exact,
    legal_actions, legal_actions_for_viewer, legal_actions_full, validated_candidate_actions,
};
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{load_and_hydrate_decks, DeckPayload};
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::printed_cards::apply_card_face_to_object;
use engine::game::scenario::{P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn dandan(db: &CardDatabase) -> GameState {
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
    start_game_with_starting_player(&mut state, P0);
    state
}

fn standard() -> GameState {
    let mut state = GameState::new_two_player(7);
    for seat in [P0, P1] {
        for i in 0..20 {
            create_object(
                &mut state,
                CardId(seat.0 as u64 * 100 + i),
                seat,
                format!("Card {i}"),
                Zone::Library,
            );
        }
    }
    start_game_with_starting_player(&mut state, P0);
    state
}

fn give_powder(state: &mut GameState, db: &CardDatabase, seat: PlayerId) -> ObjectId {
    let face = db
        .get_face_by_name("Serum Powder")
        .expect("Serum Powder is in the fixture");
    assert_eq!(
        face.oracle_text.as_deref(),
        Some(
            "{T}: Add {C}.\nAny time you could mulligan and this card is in your hand, you may exile all the cards from your hand, then draw that many cards. (You can do this in addition to taking mulligans.)"
        )
    );
    let id = create_object(
        state,
        CardId(9000 + seat.0 as u64),
        seat,
        face.name.clone(),
        Zone::Hand,
    );
    apply_card_face_to_object(state.objects.get_mut(&id).expect("just created"), face);
    id
}

fn powder(object_id: ObjectId) -> GameAction {
    GameAction::MulliganDecision {
        choice: MulliganChoice::UseSerumPowder { object_id },
    }
}

fn is_powder(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::MulliganDecision {
            choice: MulliganChoice::UseSerumPowder { .. }
        }
    )
}

fn viewer_list(state: &GameState, viewer: PlayerId) -> Vec<GameAction> {
    legal_actions_for_viewer(state, viewer).0
}

fn ai_issued(state: &GameState, seat: PlayerId) -> Vec<GameAction> {
    build_decision_context_for_semantic_owner(state, seat)
        .candidates
        .into_iter()
        .map(|c| c.action)
        .collect()
}

fn assert_powder_is_scoped_to_its_holder(state: &GameState, holder: PlayerId, object_id: ObjectId) {
    let keep = GameAction::MulliganDecision {
        choice: MulliganChoice::Keep,
    };
    let bystander = if holder == P0 { P1 } else { P0 };
    assert!(
        matches!(&state.waiting_for, WaitingFor::MulliganDecision { pending, .. } if pending.len() == 2),
        "reach: both seats are pending"
    );

    let unscoped: [(&str, Vec<GameAction>); 5] = [
        ("legal_actions_full", legal_actions_full(state).0),
        ("legal_actions", legal_actions(state)),
        (
            "candidate_actions",
            candidate_actions(state)
                .into_iter()
                .map(|c| c.action)
                .collect(),
        ),
        (
            "candidate_actions_exact",
            candidate_actions_exact(state)
                .into_iter()
                .map(|c| c.action)
                .collect(),
        ),
        (
            "validated_candidate_actions",
            validated_candidate_actions(state)
                .into_iter()
                .map(|c| c.action)
                .collect(),
        ),
    ];
    for (name, actions) in &unscoped {
        assert!(
            actions.contains(&keep),
            "reach: {name} enumerates the prompt"
        );
        assert!(
            !actions.iter().any(is_powder),
            "{name} must not carry a seat-specific UseSerumPowder"
        );
    }

    for seat in [holder, bystander] {
        assert!(
            viewer_list(state, seat).contains(&keep),
            "reach: Keep for {seat:?}"
        );
        assert!(
            ai_issued(state, seat).contains(&keep),
            "reach: AI Keep for {seat:?}"
        );
    }
    assert_eq!(
        viewer_list(state, holder)
            .iter()
            .filter(|a| is_powder(a))
            .collect::<Vec<_>>(),
        vec![&powder(object_id)]
    );
    assert!(!viewer_list(state, bystander).iter().any(is_powder));
    assert_eq!(
        ai_issued(state, holder)
            .iter()
            .filter(|a| is_powder(a))
            .collect::<Vec<_>>(),
        vec![&powder(object_id)]
    );
    assert!(!ai_issued(state, bystander).iter().any(is_powder));
}

#[test]
fn v1_dandan_powder_is_offered_only_to_the_seat_holding_it() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db);
    let object_id = give_powder(&mut state, db, P1);
    assert_powder_is_scoped_to_its_holder(&state, P1, object_id);
}

#[test]
fn v2_standard_powder_is_offered_only_to_the_seat_holding_it() {
    let Some(db) = shared_card_db() else { return };
    let mut state = standard();
    let object_id = give_powder(&mut state, db, P0);
    assert_powder_is_scoped_to_its_holder(&state, P0, object_id);
}

#[test]
fn v3_standard_holder_still_mulligans_with_the_powder() {
    let Some(db) = shared_card_db() else { return };
    let mut state = standard();
    let object_id = give_powder(&mut state, db, P1);
    let old_hand: Vec<ObjectId> = state.players[1].hand.iter().copied().collect();
    assert_eq!(old_hand.len(), 8, "reach: the Powder joined a full hand");

    let action = viewer_list(&state, P1)
        .into_iter()
        .find(is_powder)
        .expect("the holder's list carries the Powder");
    apply(&mut state, P1, action).expect("Powder accepted");

    assert_eq!(state.objects[&object_id].zone, Zone::Exile);
    let new_hand: Vec<ObjectId> = state.players[1].hand.iter().copied().collect();
    assert_eq!(new_hand.len(), 8);
    assert!(new_hand.iter().all(|id| !old_hand.contains(id)));
}
