//! Booster Tutor: "Open a sealed Magic booster pack, reveal the cards, and put
//! one of them into your hand."
//!
//! CR 400.11 + CR 400.11b + CR 701.20. Drives the real cast pipeline: the spell
//! resolves into an outside-the-game choice listing the whole opened pack, the
//! controller takes one card, that card becomes a real card in their hand, and
//! every other card in the pack goes NOWHERE — not exile, not a graveyard. They
//! were never in a zone (CR 400.11: "Outside the game is not a zone").
//!
//! The shelf is installed directly rather than stocked from the card database:
//! `boosters::build_shelf` has its own unit tests, and a synthetic product keeps
//! this test's assertions about pack contents exact.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::{GameAction, OutsideGameSelection};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{
    BoosterProduct, BoosterShelf, OutsideGameChoiceSource, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::sync::Arc;

/// Verbatim Oracle text (reminder text stripped by the card-data pipeline).
const BOOSTER_TUTOR_ORACLE: &str =
    "Open a sealed Magic booster pack, reveal the cards, and put one of them into your hand.";

const PACK_SET: &str = "TST";

fn face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            core_types: vec![CoreType::Creature],
            ..Default::default()
        },
        ..Default::default()
    }
}

/// One product with enough distinct cards to fill every slot of a pack, so the
/// collated pack is exactly the full skeleton (10 commons + 3 uncommons + 1
/// rare) with no short deal.
fn test_shelf() -> BoosterShelf {
    BoosterShelf {
        products: vec![BoosterProduct {
            set_code: PACK_SET.to_string(),
            commons: (0..20).map(|i| face(&format!("Test Common {i}"))).collect(),
            uncommons: (0..8)
                .map(|i| face(&format!("Test Uncommon {i}")))
                .collect(),
            rares: (0..4).map(|i| face(&format!("Test Rare {i}"))).collect(),
            mythics: Vec::new(),
        }],
    }
}

fn black_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![]); count]
}

/// Cast Booster Tutor and stop at the pack choice.
fn cast_and_open_pack() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(1));
    let mut runner = scenario.build();
    runner.state_mut().booster_shelf = Arc::new(test_shelf());
    let outcome = runner.cast(tutor).resolve();
    (GameRunner::from_state(outcome.state().clone()), tutor)
}

#[test]
fn booster_tutor_opens_a_pack_and_offers_every_revealed_card() {
    let (runner, _tutor) = cast_and_open_pack();

    let WaitingFor::OutsideGameChoice {
        player,
        choices,
        count,
        up_to,
        destination,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "opening a pack must raise an outside-the-game choice, got {:?}",
            runner.state().waiting_for
        );
    };

    assert_eq!(player, P0, "the spell's controller opens the pack");
    // CR 400.11b: "put ONE of them into your hand" — exactly one, not "up to".
    assert_eq!(count, 1);
    assert!(!up_to);
    assert_eq!(destination, Zone::Hand);

    // The whole pack is offered: the modern draft-booster skeleton.
    assert_eq!(choices.len(), 14, "10 commons + 3 uncommons + 1 rare");

    let mut names: Vec<&str> = Vec::new();
    for choice in &choices {
        let OutsideGameChoiceSource::BoosterPack { set_code, card, .. } = &choice.source else {
            panic!("every candidate comes from the opened pack, got {choice:?}");
        };
        assert_eq!(set_code, PACK_SET);
        assert_eq!(choice.count, 1, "each pack card is one physical card");
        names.push(card.name.as_str());
    }
    let distinct: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        names.len(),
        "a pack never contains the same card twice: {names:?}"
    );
}

#[test]
fn taking_one_card_puts_only_that_card_into_hand_and_removes_the_rest() {
    let (mut runner, tutor) = cast_and_open_pack();

    let WaitingFor::OutsideGameChoice { choices, .. } = runner.state().waiting_for.clone() else {
        panic!("expected the pack choice");
    };
    let (taken_slot, taken_name) = choices
        .iter()
        .find_map(|choice| match &choice.source {
            OutsideGameChoiceSource::BoosterPack {
                pack_slot, card, ..
            } => Some((*pack_slot, card.name.clone())),
            _ => None,
        })
        .expect("the pack offers at least one card");
    let objects_before = runner.state().objects.len();

    runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::BoosterPack {
                pack_slot: taken_slot,
            }],
        })
        .expect("taking one card from the opened pack is legal");

    // CR 400.11b: the taken card is now a real card in the controller's hand.
    let hand: Vec<&str> = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .map(|object| object.name.as_str())
        .collect();
    assert!(
        hand.contains(&taken_name.as_str()),
        "the chosen card must be in hand, hand is {hand:?}"
    );

    // CR 400.11: the other thirteen cards were never in a zone. Exactly ONE new
    // object exists — the taken card — and nothing landed in exile or a
    // graveyard. The spell itself has already left the stack for its owner's
    // graveyard (CR 608.2m), so the graveyard check excludes it.
    assert_eq!(
        runner.state().objects.len(),
        objects_before + 1,
        "only the taken card becomes an object"
    );
    assert!(
        runner.state().exile.is_empty(),
        "unchosen pack cards are not exiled"
    );
    let graveyard: Vec<&str> = runner.state().players[P0.0 as usize]
        .graveyard
        .iter()
        .filter(|id| **id != tutor)
        .filter_map(|id| runner.state().objects.get(id))
        .map(|object| object.name.as_str())
        .collect();
    assert!(
        graveyard.is_empty(),
        "unchosen pack cards are not put into a graveyard, found {graveyard:?}"
    );
}

#[test]
fn a_pack_slot_that_was_not_offered_is_rejected() {
    let (mut runner, _tutor) = cast_and_open_pack();

    let result = runner.act(GameAction::ChooseOutsideGameCards {
        selections: vec![OutsideGameSelection::BoosterPack { pack_slot: 999 }],
    });
    assert!(
        result.is_err(),
        "a slot outside the opened pack is not a legal selection"
    );
}

/// CR 400.11 + CR 609.3: an empty shelf — the shape an AI worker holding a
/// game-scoped card subset sees — opens no pack and does as much as possible
/// (nothing), rather than failing the resolution or hanging on a prompt.
#[test]
fn an_unstocked_shelf_opens_no_pack_and_leaves_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(1));
    let mut runner = scenario.build();
    runner.state_mut().booster_shelf = Arc::new(BoosterShelf::default());

    let outcome = runner.cast(tutor).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "an unstocked shelf must not leave a dangling prompt, got {:?}",
        outcome.final_waiting_for()
    );
}
