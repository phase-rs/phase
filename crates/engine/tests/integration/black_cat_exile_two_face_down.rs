//! Black Cat, Cunning Thief: "When Black Cat enters, look at the top nine cards of target
//! opponent's library, exile two of them face down, then put the rest on the bottom of their
//! library in a random order. You may play the exiled cards for as long as they remain exiled."
//!
//! Before the fix "exile two of them face down" was not recognised (only the singular
//! "exile one of them face down" was), so the look-step `Dig` stayed a `keep_count: 0` peek and
//! the chain carried no count for the exile, no face-down marker and a `count: 1` library
//! placement for "the rest". The plural now fuses into the same Hideaway-shaped `Dig`
//! (`keep_count: 2`, destination Exile, chained conceal) the singular uses, and "the rest"
//! is the `Dig`'s own remainder.
//!
//! The two zone counts guard each other: exactly two cards reach exile AND exactly seven
//! reach the bottom of the library, so neither can be satisfied by moving everything one place.
//!
//! CR 701.20e: looking at cards is private. CR 406.3: a card exiled face down can't be examined.
//! CR 401.4: cards put in a specific library position at once may be arranged in any order
//! (here, randomly).

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const BLACK_CAT_ORACLE: &str = "When Black Cat enters, look at the top nine cards of target opponent's library, exile two of them face down, then put the rest on the bottom of their library in a random order. You may play the exiled cards for as long as they remain exiled. Mana of any type can be spent to cast spells this way.";

fn library(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[P1.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

#[test]
fn black_cat_exiles_exactly_two_face_down_and_bottoms_the_other_seven() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Eleven cards: the top nine are looked at; the two deeper ones must be untouched.
    let mut ids = Vec::new();
    for i in (1..=11).rev() {
        ids.push(scenario.add_card_to_library_top(P1, &format!("Lib Card {i}")));
    }
    ids.reverse(); // ids[0] is the top card, ids[10] the deepest
    let looked_at_ids: Vec<ObjectId> = ids[..9].to_vec();
    let deeper_ids: Vec<ObjectId> = ids[9..].to_vec();

    let cat = scenario
        .add_creature_to_hand_from_oracle(P0, "Black Cat, Cunning Thief", 1, 1, BLACK_CAT_ORACLE)
        .with_mana_cost(ManaCost::default())
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&cat].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: cat,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");

    let mut offered: Option<Vec<ObjectId>> = None;
    let mut chosen: Vec<ObjectId> = Vec::new();
    let mut passed = false;
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Player(P1))
                    .cloned();
                assert!(choice.is_some(), "the opponent must be a legal target");
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::DigChoice {
                player,
                library_owner,
                cards,
                keep_count,
                ..
            } => {
                assert_eq!(player, P0, "Black Cat's controller makes the selection");
                assert_eq!(library_owner, P1, "the dug library is the opponent's");
                assert_eq!(keep_count, 2, "Black Cat keeps exactly two looked-at cards");
                chosen = vec![cards[0], cards[1]];
                offered = Some(cards.clone());
                runner
                    .act(GameAction::SelectCards {
                        cards: chosen.clone(),
                    })
                    .expect("SelectCards accepted");
            }
            WaitingFor::Priority { .. } => {
                // Stop once the spell and its trigger have both resolved (empty stack after
                // at least one pass) so a missing DigChoice fails below, not as a deck-out.
                if runner.state().stack.is_empty() && passed {
                    break;
                }
                passed = true;
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while driving Black Cat's ETB: {other:?}"),
        }
    }

    let offered = offered.expect(
        "Black Cat's ETB must surface a DigChoice for the two exiled cards; before the fix the \
         look-step was a keep_count:0 peek and no choice was offered",
    );
    assert_eq!(offered.len(), 9, "the dig looks at exactly the top nine");
    assert!(
        offered.iter().all(|id| looked_at_ids.contains(id)),
        "only the top nine are looked at"
    );

    let state = runner.state();
    let in_exile: Vec<ObjectId> = ids
        .iter()
        .copied()
        .filter(|id| state.objects[id].zone == Zone::Exile)
        .collect();
    // Reach-guard and negative assertion in one test: exactly the two chosen cards are exiled...
    assert_eq!(in_exile.len(), 2, "exactly two cards reach exile");
    for id in &chosen {
        assert_eq!(state.objects[id].zone, Zone::Exile);
        assert!(
            state.objects[id].face_down,
            "the exiled cards are face down (CR 406.3)"
        );
    }

    // ...and exactly the other seven looked-at cards are the bottom seven of the library,
    // beneath the two cards that were never looked at.
    let rest: Vec<ObjectId> = looked_at_ids
        .iter()
        .copied()
        .filter(|id| !chosen.contains(id))
        .collect();
    assert_eq!(rest.len(), 7);
    let lib = library(&runner);
    assert_eq!(lib.len(), 9, "two deeper cards plus the seven rest cards");
    assert_eq!(
        &lib[..2],
        &deeper_ids[..],
        "unlooked cards stay on top of the rest"
    );
    let mut bottom: Vec<ObjectId> = lib[2..].to_vec();
    bottom.sort_by_key(|id| id.0);
    let mut expected_rest = rest;
    expected_rest.sort_by_key(|id| id.0);
    assert_eq!(
        bottom, expected_rest,
        "exactly the other seven are bottomed"
    );

    // "in a random order": the bottom pile must not be the looked-at (pre-dig) order. The
    // scenario seed is fixed, so this is deterministic and flips if the rest is placed in
    // `Preserve` order.
    let pre_dig_order: Vec<ObjectId> = looked_at_ids
        .iter()
        .copied()
        .filter(|id| !chosen.contains(id))
        .collect();
    assert_ne!(
        lib[2..].to_vec(),
        pre_dig_order,
        "the bottom seven must be randomized, not left in looked-at order"
    );

    // "You may play the exiled cards": both exiled cards are playable by P0.
    let castable = spell_objects_available_to_cast(state, P0);
    for id in &chosen {
        assert!(castable.contains(id), "the controller may play {id:?}");
    }
}
