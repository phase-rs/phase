//! Integration test: Boneyard Parley pile-separation flow (ExiledThisWay).
//!
//! Verifies the production Oracle parser path end-to-end:
//! 1. Parse Boneyard Parley Oracle text → exile + `SeparateIntoPiles` effect.
//! 2. Cast it → cards exiled from graveyards.
//! 3. An opponent separates those exiled cards into two piles.
//! 4. Controller chooses a pile → chosen pile goes to battlefield, unchosen to
//!    owners' graveyards.

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

/// Full Boneyard Parley flow driven through the production Oracle parser.
///
/// Boneyard Parley exiles up to 5 target creature cards from graveyards, then
/// an opponent separates them into two piles. The controller puts all cards
/// from one pile onto the battlefield under their control and the rest into
/// their owners' graveyards.
#[test]
fn boneyard_parley_full_flow() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Seed P0's graveyard with 3 creature cards.
    let gy_cards: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature_to_graveyard(P0, &format!("GYCreature{i}"), 2, 2)
                .id()
        })
        .collect();

    // Boneyard Parley costs {5}{B}{B} — 5 generic + 2 black.
    let oracle_text = "Exile up to five target creature cards from graveyards. \
                       An opponent separates those cards into two piles. \
                       Put all cards from the pile of your choice onto the \
                       battlefield under your control and the rest into their \
                       owners' graveyards.";
    let spell_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Boneyard Parley", false, oracle_text);
    let spell_id = spell_builder.id();

    // Give P0 enough mana.
    scenario.with_mana_pool(P0, floating_mana(5, 2));

    let mut runner = scenario.build();

    // Cast Boneyard Parley targeting the 3 graveyard creatures.
    let outcome = runner.cast(spell_id).target_objects(&gy_cards).resolve();

    // After resolution, we expect SeparatePilesPartition for P1 (the opponent).
    match outcome.final_waiting_for() {
        WaitingFor::SeparatePilesPartition {
            player, eligible, ..
        } => {
            assert_eq!(*player, P1, "opponent should be the partitioner");
            assert_eq!(eligible.len(), 3, "should have 3 exiled cards");
        }
        other => panic!("expected SeparatePilesPartition, got {other:?}"),
    }

    // P1 separates: first 2 in pile A, last 1 in pile B.
    let eligible_ids: Vec<ObjectId> = match &runner.state().waiting_for {
        WaitingFor::SeparatePilesPartition { eligible, .. } => eligible.iter().copied().collect(),
        _ => panic!("expected SeparatePilesPartition"),
    };
    let pile_a_ids: Vec<ObjectId> = eligible_ids[0..2].to_vec();
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

    // P0 chooses pile A (the 2-card pile) — those go to battlefield.
    runner
        .act(GameAction::ChoosePile { pile: PileSide::A })
        .expect("pile choice accepted");

    // Verify: chosen pile (A = first 2 cards) should be on the battlefield
    // under the CASTER's control (P0), per CR 110.2a.
    for &card_id in &pile_a_ids {
        assert!(
            runner.state().battlefield.contains(&card_id),
            "chosen pile card {card_id:?} should be on the battlefield"
        );
        let obj = runner
            .state()
            .objects
            .get(&card_id)
            .expect("card should exist");
        assert_eq!(
            obj.controller, P0,
            "CR 110.2a: chosen pile card {card_id:?} must enter under the caster's control"
        );
    }

    // Verify: unchosen pile (B = last card) should be in owner's graveyard.
    let gy = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .graveyard;
    for &card_id in &eligible_ids[2..] {
        assert!(
            gy.contains(&card_id),
            "unchosen pile card {card_id:?} should be in owner's graveyard"
        );
    }

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}
