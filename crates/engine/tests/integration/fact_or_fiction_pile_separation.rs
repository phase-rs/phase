//! Integration test: Fact or Fiction pile-separation flow.
//!
//! Verifies the production Oracle parser path end-to-end:
//! 1. Parse Fact or Fiction Oracle text → `SeparateIntoPiles` effect.
//! 2. Cast it → top 5 cards revealed.
//! 3. An opponent separates them into two piles.
//! 4. Controller chooses a pile → chosen pile goes to hand, unchosen to graveyard.

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

/// Full Fact or Fiction flow driven through the production Oracle parser.
#[test]
fn fact_or_fiction_full_flow() {
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

    // Use the Oracle parser path — the production card-data pipeline parses
    // this exact text into Effect::SeparateIntoPiles with
    // PileSource::RevealedFromLibraryTop { count: 5 }.
    let oracle_text = "Reveal the top five cards of your library. \
                       An opponent separates those cards into two piles. \
                       Put one pile into your hand and the rest into your graveyard.";
    let fof_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Fact or Fiction", true, oracle_text);
    let fof_id = fof_builder.id();

    // Give P0 enough mana (3 generic + 1 blue = {3}{U}).
    scenario.with_mana_pool(P0, floating_mana(3, 1));

    let mut runner = scenario.build();

    // Cast Fact or Fiction and resolve.
    let outcome = runner.cast(fof_id).resolve();

    // After resolution, we expect SeparatePilesPartition for P1 (the opponent).
    match outcome.final_waiting_for() {
        WaitingFor::SeparatePilesPartition {
            player, eligible, ..
        } => {
            assert_eq!(*player, P1, "opponent should be the partitioner");
            assert_eq!(eligible.len(), 5, "should have 5 revealed cards");
        }
        other => panic!("expected SeparatePilesPartition, got {other:?}"),
    }

    // P1 separates: first 3 in pile A, last 2 in pile B.
    let pile_a_ids: Vec<_> = lib_cards[0..3].to_vec();
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

    // P0 chooses pile A (the 3-card pile).
    runner
        .act(GameAction::ChoosePile { pile: PileSide::A })
        .expect("pile choice accepted");

    // Verify: chosen pile (A = first 3 cards) should be in P0's hand.
    let hand = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand;
    for &card_id in &pile_a_ids {
        assert!(
            hand.contains(&card_id),
            "chosen pile card {card_id:?} should be in hand"
        );
    }

    // Verify: unchosen pile (B = last 2 cards) should be in P0's graveyard.
    let gy = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .graveyard;
    for &card_id in &lib_cards[3..5] {
        assert!(
            gy.contains(&card_id),
            "unchosen pile card {card_id:?} should be in graveyard"
        );
    }
}
