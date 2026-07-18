//! Integration test: Make an Example pile-separation flow (Battlefield).
//!
//! Regression guard for the Battlefield `PileSource` path. Verifies:
//! 1. Parse Make an Example Oracle text → `SeparateIntoPiles` effect.
//! 2. Cast it → opponent partitions their creatures.
//! 3. Controller chooses a pile → chosen pile is sacrificed.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{PileSide, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

fn floating_mana(generic: usize, black: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..black {
        pool.push(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
    }
    pool
}

/// Full Make an Example flow driven through the production Oracle parser.
///
/// Make an Example: Each opponent separates the creatures they control into
/// two piles. For each opponent, you choose one of their piles. Each creature
/// in the chosen piles is sacrificed.
#[test]
fn make_an_example_full_flow() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 controls 3 creatures.
    let c1 = scenario.add_creature(P1, "Bear", 2, 2).id();
    let c2 = scenario.add_creature(P1, "Wolf", 3, 3).id();
    let c3 = scenario.add_creature(P1, "Elk", 3, 3).id();

    // Make an Example costs {3}{B}.
    let oracle_text = "Each opponent separates the creatures they control into \
                       two piles. For each opponent, you choose one of their \
                       piles. Each opponent sacrifices the creatures in their chosen pile.";
    let spell_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Make an Example", false, oracle_text);
    let spell_id = spell_builder.id();

    // Give P0 enough mana.
    scenario.with_mana_pool(P0, floating_mana(3, 1));

    let mut runner = scenario.build();

    // Cast Make an Example and resolve.
    let outcome = runner.cast(spell_id).resolve();

    // After resolution, P1 should be partitioning their creatures.
    match outcome.final_waiting_for() {
        WaitingFor::SeparatePilesPartition {
            player, eligible, ..
        } => {
            assert_eq!(*player, P1, "opponent should be the partitioner");
            assert_eq!(eligible.len(), 3, "should have 3 creatures");
            assert!(eligible.contains(&c1));
            assert!(eligible.contains(&c2));
            assert!(eligible.contains(&c3));
        }
        other => panic!("expected SeparatePilesPartition, got {other:?}"),
    }

    // P1 separates: c1, c2 in pile A; c3 in pile B.
    runner
        .act(GameAction::SubmitPilePartition {
            pile_a: vec![c1, c2],
        })
        .expect("partition accepted");

    // Now P0 should be choosing a pile.
    match &runner.state().waiting_for {
        WaitingFor::SeparatePilesChoice { player, .. } => {
            assert_eq!(*player, P0, "controller should choose");
        }
        other => panic!("expected SeparatePilesChoice, got {other:?}"),
    }

    // P0 chooses pile A (c1, c2) — those are sacrificed.
    runner
        .act(GameAction::ChoosePile { pile: PileSide::A })
        .expect("pile choice accepted");

    // Verify: chosen pile (c1, c2) should be sacrificed (moved to graveyard).
    assert!(
        !runner.state().battlefield.contains(&c1),
        "c1 should be sacrificed"
    );
    assert!(
        !runner.state().battlefield.contains(&c2),
        "c2 should be sacrificed"
    );
    // Verify: unchosen pile (c3) should remain on battlefield.
    assert!(
        runner.state().battlefield.contains(&c3),
        "c3 should remain on battlefield"
    );

    // Verify sacrificed creatures are in graveyard.
    let gy = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .graveyard;
    assert!(gy.contains(&c1), "c1 should be in graveyard");
    assert!(gy.contains(&c2), "c2 should be in graveyard");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}
