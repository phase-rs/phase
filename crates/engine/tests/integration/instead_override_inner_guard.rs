//! CR 608.2c + CR 614.6: an "instead" override's selector chooses the
//! replacement; a guard the replacement body prints on its own instruction still
//! gates that instruction and never restores the replaced one.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Accepted grammar with no printed card: a multi-instruction "instead" body
/// whose first instruction carries its own "if" guard.
const KICKED_GUARDED_BODY: &str =
    "Kicker {2} (You may pay an additional {2} as you cast this spell.)\n\
Draw a card. If this spell was kicked, instead draw two cards if you control three or more \
creatures, then gain 1 life.";

/// The single-instruction "If <state>, <body> instead" form with a guarded body.
const STATE_GUARDED_BODY: &str = "Draw a card. If you have 10 or less life, draw two cards if \
you control three or more creatures instead.";

/// The multi-instruction "If <state>, instead <body>" form with a guarded first instruction.
const STATE_GUARDED_MULTI_BODY: &str = "Draw a card. If you have 10 or less life, instead draw \
two cards if you control three or more creatures, then gain 1 life.";

/// The override as its own paragraph (CR 614.15), leading "instead".
const PARAGRAPH_GUARDED_BODY: &str = "Draw a card.\nIf you have 10 or less life, instead draw two \
cards if you control three or more creatures, then gain 1 life.";

/// The override as its own paragraph, trailing "instead".
const PARAGRAPH_TRAILING_GUARDED_BODY: &str = "Draw a card.\nIf you have 10 or less life, draw \
two cards if you control three or more creatures, then gain 1 life instead.";

/// A guarded override body whose instruction names the replaced one's tokens.
const KICKED_GUARDED_TOKEN_BODY: &str =
    "Kicker {2} (You may pay an additional {2} as you cast this spell.)\n\
Create a 1/1 white Soldier creature token. If this spell was kicked, instead create two of \
those tokens if you control three or more creatures, then gain 1 life.";

const FATAL_PUSH: &str =
    "Destroy target creature if it has mana value 2 or less.\nRevolt \u{2014} \
Destroy that creature if it has mana value 4 or less instead if a permanent left the battlefield \
under your control this turn.";

struct Board {
    kicked: Option<bool>,
    life: i32,
    creatures: usize,
}

/// P0 casts `oracle` on `board`; returns (cards drawn, life gained) once the stack is empty.
fn cast(oracle: &str, board: Board) -> (i64, i32) {
    let outcome = cast_outcome(oracle, board);
    (outcome.hand_drawn(P0), outcome.life_delta(P0))
}

fn cast_outcome(oracle: &str, board: Board) -> CastOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, board.life);
    let spell = scenario
        .add_spell_to_hand(P0, "Guarded Instead", false)
        .from_oracle_text_with_keywords(&["Kicker"], oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    for index in 0..board.creatures {
        scenario.add_creature(P0, &format!("Bear {index}"), 2, 2);
    }
    for index in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let cast = runner.cast(spell);
    let cast = match board.kicked {
        Some(true) => cast.accept_optional(),
        Some(false) => cast.decline_optional(),
        None => cast,
    };
    let outcome = cast.resolve();
    assert!(outcome.state().stack.is_empty(), "the spell resolved");
    outcome
}

/// Kicked, inner guard false: "draw a card" never happens (CR 614.6), "draw two
/// cards if you control three or more creatures" draws nothing, and "gain 1
/// life" still happens.
#[test]
fn kicked_guarded_body_with_false_guard_draws_nothing() {
    let outcome = cast(
        KICKED_GUARDED_BODY,
        Board {
            kicked: Some(true),
            life: 20,
            creatures: 0,
        },
    );
    assert_eq!(outcome, (0, 1));
}

/// Kicked, inner guard true: draw two cards, then gain 1 life.
#[test]
fn kicked_guarded_body_with_true_guard_draws_two_and_gains_life() {
    let outcome = cast(
        KICKED_GUARDED_BODY,
        Board {
            kicked: Some(true),
            life: 20,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (2, 1));
}

/// Unkicked: only "Draw a card." happens.
#[test]
fn unkicked_guarded_body_draws_one() {
    let outcome = cast(
        KICKED_GUARDED_BODY,
        Board {
            kicked: Some(false),
            life: 20,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (1, 0));
}

/// Selector holds, inner guard false: neither "draw a card" nor "draw two cards".
#[test]
fn state_selected_guarded_body_with_false_guard_draws_nothing() {
    let outcome = cast(
        STATE_GUARDED_BODY,
        Board {
            kicked: None,
            life: 10,
            creatures: 0,
        },
    );
    assert_eq!(outcome, (0, 0));
}

/// Selector holds, inner guard true: draw two cards.
#[test]
fn state_selected_guarded_body_with_true_guard_draws_two() {
    let outcome = cast(
        STATE_GUARDED_BODY,
        Board {
            kicked: None,
            life: 10,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (2, 0));
}

/// Selector fails: only "Draw a card." happens.
#[test]
fn unselected_guarded_body_draws_one() {
    let outcome = cast(
        STATE_GUARDED_BODY,
        Board {
            kicked: None,
            life: 20,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (1, 0));
}

/// Selector holds, inner guard false: no draw, then gain 1 life.
#[test]
fn state_selected_multi_body_with_false_guard_gains_life_only() {
    let outcome = cast(
        STATE_GUARDED_MULTI_BODY,
        Board {
            kicked: None,
            life: 10,
            creatures: 0,
        },
    );
    assert_eq!(outcome, (0, 1));
}

/// Separate override paragraph, selector holds, inner guard false.
#[test]
fn paragraph_override_with_false_guard_gains_life_only() {
    let outcome = cast(
        PARAGRAPH_GUARDED_BODY,
        Board {
            kicked: None,
            life: 10,
            creatures: 0,
        },
    );
    assert_eq!(outcome, (0, 1));
}

/// Separate override paragraph, selector holds, inner guard true.
#[test]
fn paragraph_override_with_true_guard_draws_two_and_gains_life() {
    let outcome = cast(
        PARAGRAPH_GUARDED_BODY,
        Board {
            kicked: None,
            life: 10,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (2, 1));
}

/// Separate override paragraph, selector fails: only "Draw a card." happens.
#[test]
fn paragraph_override_unselected_draws_one() {
    let outcome = cast(
        PARAGRAPH_GUARDED_BODY,
        Board {
            kicked: None,
            life: 20,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (1, 0));
}

/// Trailing-"instead" paragraph, selector holds, inner guard false.
#[test]
fn trailing_paragraph_override_with_false_guard_gains_life_only() {
    let outcome = cast(
        PARAGRAPH_TRAILING_GUARDED_BODY,
        Board {
            kicked: None,
            life: 10,
            creatures: 0,
        },
    );
    assert_eq!(outcome, (0, 1));
}

/// Trailing-"instead" paragraph, selector fails: only "Draw a card." happens.
#[test]
fn trailing_paragraph_override_unselected_draws_one() {
    let outcome = cast(
        PARAGRAPH_TRAILING_GUARDED_BODY,
        Board {
            kicked: None,
            life: 20,
            creatures: 3,
        },
    );
    assert_eq!(outcome, (1, 0));
}

/// CR 111.1: "two of those tokens" names the replaced instruction's Soldier
/// even beneath the guarded body.
#[test]
fn kicked_guarded_token_body_creates_two_soldiers() {
    let outcome = cast_outcome(
        KICKED_GUARDED_TOKEN_BODY,
        Board {
            kicked: Some(true),
            life: 20,
            creatures: 3,
        },
    );
    let soldiers = outcome
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let object = &outcome.state().objects[*id];
            object.is_token && object.name == "Soldier"
        })
        .count();
    assert_eq!((soldiers, outcome.life_delta(P0)), (2, 1));
}

/// P0 casts Fatal Push at a P1 creature of mana value `mana_value`; true when it was destroyed.
fn fatal_push_destroys(mana_value: u32, revolt: bool) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Fatal Push", true)
        .from_oracle_text(FATAL_PUSH)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let victim = scenario
        .add_creature(P1, "Victim", 2, 2)
        .with_mana_cost(ManaCost::generic(mana_value))
        .id();
    let leaver = scenario.add_creature(P0, "Leaver", 1, 1).id();
    let mut runner = scenario.build();
    if revolt {
        move_to_zone(runner.state_mut(), leaver, Zone::Graveyard, &mut Vec::new());
    }
    let outcome = runner.cast(spell).target_object(victim).resolve();
    assert!(outcome.state().stack.is_empty(), "Fatal Push resolved");
    outcome.zone_of(victim) == Zone::Graveyard
}

/// Fatal Push: revolt selects the override, whose own mana-value guard still applies.
#[test]
fn fatal_push_revolt_override_keeps_its_own_guard() {
    assert!(fatal_push_destroys(3, true));
    assert!(!fatal_push_destroys(5, true));
    assert!(!fatal_push_destroys(3, false));
    assert!(fatal_push_destroys(2, false));
}
