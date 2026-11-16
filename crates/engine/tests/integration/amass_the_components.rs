//! Tests for Amass the Components:
//! "Draw three cards, then put a card from your hand on the bottom of your library."
//!
//! Verifies:
//! - CR 608.2c: Draw three cards, then prompt for a card from hand to place on bottom of library.
//! - Card is placed on the bottom of its owner's library (back of library VecDeque).

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, LibraryPosition, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AMASS_THE_COMPONENTS_ORACLE: &str =
    "Draw three cards, then put a card from your hand on the bottom of your library.";

#[test]
fn shape_amass_the_components_ast() {
    let parsed = parse_oracle_text(
        AMASS_THE_COMPONENTS_ORACLE,
        "Amass the Components",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert_eq!(
        parsed.abilities.len(),
        1,
        "Amass the Components must have exactly one ability"
    );
    let root = &parsed.abilities[0];
    let Effect::Draw { count, target } = &*root.effect else {
        panic!("expected Draw root effect, got {:?}", root.effect);
    };
    assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
    assert_eq!(*target, TargetFilter::Controller);

    let sub = root.sub_ability.as_ref().expect("must have sub_ability");
    let Effect::PutAtLibraryPosition {
        count, position, ..
    } = &*sub.effect
    else {
        panic!(
            "expected PutAtLibraryPosition sub-ability, got {:?}",
            sub.effect
        );
    };
    assert_eq!(*position, LibraryPosition::Bottom);
    assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
}

#[test]
fn amass_the_components_puts_card_on_bottom_of_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Setup library: Card 1 (bottom), Card 2, Card 3, Card 4 (top)
    let bottom_card = scenario.add_card_to_library_top(P0, "Lib Bottom");
    let _lib2 = scenario.add_card_to_library_top(P0, "Lib 2");
    let _lib3 = scenario.add_card_to_library_top(P0, "Lib 3");
    let _lib4 = scenario.add_card_to_library_top(P0, "Lib 4");

    let amass = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Amass the Components",
            false,
            AMASS_THE_COMPONENTS_ORACLE,
        )
        .id();

    let mut runner = scenario.build();

    let mut commit = runner.cast(amass).commit();

    // Advance until we get to WaitingFor::EffectZoneChoice
    for _ in 0..20 {
        match commit.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                commit.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for before EffectZoneChoice: {other:?}"),
        }
    }

    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        library_position,
        ..
    } = commit.state().waiting_for.clone()
    else {
        panic!(
            "expected EffectZoneChoice, got {:?}",
            commit.state().waiting_for
        );
    };

    assert_eq!(player, P0);
    assert_eq!(count, 1);
    assert_eq!(library_position, Some(LibraryPosition::Bottom));

    // Pick one of the cards in hand to put on the bottom of library.
    let chosen_card = cards[0];
    commit
        .act(GameAction::SelectCards {
            cards: vec![chosen_card],
        })
        .expect("select card to put on bottom");

    let p0_lib = &commit.state().players[P0.0 as usize].library;
    assert_eq!(
        commit.state().objects[&chosen_card].zone,
        Zone::Library,
        "chosen card must be moved to Library"
    );

    // Card put on the bottom of library must be at the bottom (back of VecDeque), NOT the top.
    assert_eq!(
        p0_lib.front().copied(),
        Some(bottom_card),
        "existing bottom card was the only card not drawn, so it is now on top"
    );
    assert_eq!(
        p0_lib.back().copied(),
        Some(chosen_card),
        "chosen card must be on the bottom of the library"
    );
    assert_ne!(
        p0_lib.front().copied(),
        Some(chosen_card),
        "chosen card must NOT be on top of the library"
    );
}

/// CR 401.4: When two cards are placed on the bottom of a library simultaneously,
/// selecting [A, B] places A first and B second, so B ends at the very bottom
/// (back of VecDeque) and A ends immediately above B (second from bottom).
#[test]
fn two_cards_put_on_bottom_of_library_preserves_engine_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bottom_card = scenario.add_card_to_library_top(P0, "Original Bottom");
    let _lib2 = scenario.add_card_to_library_top(P0, "Lib 2");
    let _lib3 = scenario.add_card_to_library_top(P0, "Lib 3");
    let _lib4 = scenario.add_card_to_library_top(P0, "Lib 4");

    let valakut = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Valakut Awakening",
            false,
            "Put any number of cards from your hand on the bottom of your library, then draw that many cards plus one.",
        )
        .id();
    let card_a = scenario.add_card_to_hand(P0, "Hand Card A");
    let card_b = scenario.add_card_to_hand(P0, "Hand Card B");

    let mut runner = scenario.build();
    let mut commit = runner.cast(valakut).commit();

    for _ in 0..20 {
        match commit.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                commit.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for before EffectZoneChoice: {other:?}"),
        }
    }

    let WaitingFor::EffectZoneChoice {
        player,
        library_position,
        ..
    } = commit.state().waiting_for.clone()
    else {
        panic!(
            "expected EffectZoneChoice, got {:?}",
            commit.state().waiting_for
        );
    };

    assert_eq!(player, P0);
    assert_eq!(library_position, Some(LibraryPosition::Bottom));

    // Select A then B
    commit
        .act(GameAction::SelectCards {
            cards: vec![card_a, card_b],
        })
        .expect("select cards to put on bottom");

    let p0_lib = &commit.state().players[P0.0 as usize].library;
    assert_eq!(
        commit.state().objects[&card_a].zone,
        Zone::Library,
        "card A must be moved to Library"
    );
    assert_eq!(
        commit.state().objects[&card_b].zone,
        Zone::Library,
        "card B must be moved to Library"
    );

    // CR 401.4: Card B was selected after Card A; the engine appends bottom
    // placements in submission order, so Card B is at the very bottom (back of VecDeque),
    // and Card A is immediately above it (second from bottom).
    let lib_len = p0_lib.len();
    assert_eq!(
        p0_lib.back().copied(),
        Some(card_b),
        "card B must be at the very bottom of the library"
    );
    assert_eq!(
        p0_lib.get(lib_len - 2).copied(),
        Some(card_a),
        "card A must be second from bottom (immediately above card B)"
    );
    assert_eq!(
        p0_lib.front().copied(),
        Some(bottom_card),
        "existing bottom card remains above the newly placed bottom cards"
    );
}
