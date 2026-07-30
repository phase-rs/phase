//! Integration test: Sphinx of Uthuun ETB pile-separation flow.
//!
//! Verifies that the Fact-or-Fiction pile-separation effect works when it
//! appears as an ETB trigger body (not just as a spell body). Sphinx of Uthuun
//! is the canonical creature with this pattern:
//!
//! ```text
//! Flying
//! When this creature enters, reveal the top five cards of your library.
//! An opponent separates those cards into two piles. Put one pile into your
//! hand and the other into your graveyard.
//! ```
//!
//! The test drives the REAL pipeline: a creature built from Oracle text,
//! entering the battlefield, triggering, and resolving the full pile-separation
//! interactive flow (opponent partitions → controller chooses → zone routing).
//!
//! CR 700.3: Pile-separation rules.
//! CR 701.20a: Reveal is a keyword action.
//! CR 603.1: Triggered abilities trigger when events occur.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{PileSide, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

fn floating_mana(generic: usize, blue: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..blue {
        pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
    pool
}

/// Full Sphinx of Uthuun ETB pile-separation flow driven through the
/// production Oracle parser. The trigger body must route through
/// `parse_separate_into_piles` to produce a single `SeparateIntoPiles` effect
/// rather than fragmenting into Unimplemented chunks.
#[test]
fn sphinx_of_uthuun_etb_pile_separation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Seed library with 5 known cards (top to bottom: C0..C4).
    // add_card_to_library_top inserts at index 0, so we add in reverse order
    // so that LibCard0 ends up on top.
    let mut lib_cards = Vec::new();
    for i in (0..5).rev() {
        let id = scenario.add_card_to_library_top(P0, &format!("LibCard{i}"));
        lib_cards.push(id);
    }
    lib_cards.reverse(); // now lib_cards[0] = top card (LibCard0)

    // The Sphinx's Oracle text — the ETB trigger body contains the full
    // Fact-or-Fiction pile-separation pattern.
    let oracle_text = "Flying\n\
                       When this creature enters, reveal the top five cards of your library. \
                       An opponent separates those cards into two piles. \
                       Put one pile into your hand and the other into your graveyard.";

    // Add Sphinx of Uthuun to hand as a creature so it can enter the
    // battlefield and trigger its ETB ability.
    let sphinx_id = scenario
        .add_creature_to_hand_from_oracle(P0, "Sphinx of Uthuun", 5, 6, oracle_text)
        .id();

    // Give P0 enough mana (5 generic + 2 blue = {5}{U}{U}).
    scenario.with_mana_pool(P0, floating_mana(5, 2));

    let mut runner = scenario.build();

    // Cast Sphinx of Uthuun — it enters the battlefield and triggers.
    let outcome = runner.cast(sphinx_id).resolve();

    // After the ETB trigger resolves, we expect SeparatePilesPartition for P1.
    match outcome.final_waiting_for() {
        WaitingFor::SeparatePilesPartition {
            player, eligible, ..
        } => {
            assert_eq!(*player, P1, "opponent should be the partitioner");
            assert_eq!(eligible.len(), 5, "should have 5 revealed cards");
        }
        other => panic!("expected SeparatePilesPartition, got {other:?}"),
    }

    // P1 separates: first 2 in pile A, last 3 in pile B.
    let pile_a_ids: Vec<_> = lib_cards[0..2].to_vec();
    runner
        .act(GameAction::SubmitPilePartition {
            pile_a: pile_a_ids.clone(),
        })
        .expect("partition accepted");

    // Now P0 should be choosing a pile.
    match &runner.state().waiting_for {
        WaitingFor::SeparatePilesChoice { player, .. } => {
            assert_eq!(*player, P0, "controller should choose");
        }
        other => panic!("expected SeparatePilesChoice, got {other:?}"),
    }

    // P0 chooses pile B (the 3-card pile).
    runner
        .act(GameAction::ChoosePile { pile: PileSide::B })
        .expect("pile choice accepted");

    // Verify: chosen pile (B = last 3 cards) should be in P0's hand.
    let hand = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand;
    for &card_id in &lib_cards[2..5] {
        assert!(
            hand.contains(&card_id),
            "chosen pile card {card_id:?} should be in hand"
        );
    }

    // Verify: unchosen pile (A = first 2 cards) should be in P0's graveyard.
    let gy = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .graveyard;
    for &card_id in &lib_cards[0..2] {
        assert!(
            gy.contains(&card_id),
            "unchosen pile card {card_id:?} should be in graveyard"
        );
    }

    // Game should return to priority.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}
