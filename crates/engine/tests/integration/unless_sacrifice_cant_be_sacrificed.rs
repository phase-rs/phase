//! CR 101.2 + CR 701.21a: a permanent that can't be sacrificed can't pay an
//! "unless they sacrifice ..." cost. Alexios, Deimos of Kosmos ("can't be
//! sacrificed"; ruling: "You also can't sacrifice it to pay a cost that
//! requires you to sacrifice a creature.") is never offered as the pick of the
//! generic unless-payment sacrifice path.
//!
//! Mogis, God of Slaughter: "At the beginning of each opponent's upkeep, Mogis
//! deals 2 damage to that player unless they sacrifice a creature of their
//! choice." The Mogis controller's opponent is the active player here, so
//! Mogis is P1's and the Alexios is P0's, whose upkeep it is.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Mogis under P1, Alexios under P0 (plus a Grizzly Bears when
/// `with_bears`), advanced to P0's upkeep with every trigger on the stack.
fn mogis_at_the_opponents_upkeep(
    with_bears: bool,
) -> Option<(GameRunner, ObjectId, Option<ObjectId>)> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_real_card(P1, "Mogis, God of Slaughter", Zone::Battlefield, db);
    let alexios = scenario.add_real_card(P0, "Alexios, Deimos of Kosmos", Zone::Battlefield, db);
    let bears = with_bears.then(|| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    Some((runner, alexios, bears))
}

/// Answer every unless-payment prompt of Mogis by paying, asserting that no
/// pick ever offers `alexios`; returns the picks offered.
fn pay_and_collect_picks(runner: &mut GameRunner, alexios: ObjectId) -> Vec<Vec<ObjectId>> {
    let mut picks = Vec::new();
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::UnlessPayment { player, .. } if player == P0 => {
                runner
                    .act(GameAction::PayUnlessCost { pay: true })
                    .expect("P0 chooses to pay");
            }
            WaitingFor::WardSacrificeChoice { permanents, .. } => {
                assert!(
                    !permanents.contains(&alexios),
                    "Alexios can't be sacrificed: {permanents:?}"
                );
                picks.push(permanents.clone());
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![permanents[0]],
                    })
                    .expect("the pick is accepted");
            }
            _ => break,
        }
    }
    picks
}

/// CR 101.2: with only Alexios, P0 has nothing to sacrifice, so Mogis's damage
/// is dealt without a payment prompt.
#[test]
fn mogis_unless_pool_excludes_a_permanent_that_cant_be_sacrificed() {
    let Some((mut runner, alexios, _)) = mogis_at_the_opponents_upkeep(false) else {
        return;
    };

    let picks = pay_and_collect_picks(&mut runner, alexios);
    runner.advance_until_stack_empty();

    assert!(picks.is_empty(), "no pick was offered: {picks:?}");
    assert_eq!(runner.life(P0), 18, "Mogis dealt its 2 damage");
    assert_eq!(
        runner.state().objects[&alexios].zone,
        Zone::Battlefield,
        "Alexios was not sacrificed"
    );
}

/// Paired reach guard: with a Grizzly Bears beside Alexios, the payment is
/// possible and offers exactly the Bears.
#[test]
fn mogis_unless_pool_offers_the_sacrificeable_creature_beside_one_that_cant_be() {
    let Some((mut runner, alexios, bears)) = mogis_at_the_opponents_upkeep(true) else {
        return;
    };
    let bears = bears.expect("the Bears were added");

    let picks = pay_and_collect_picks(&mut runner, alexios);
    runner.advance_until_stack_empty();

    assert_eq!(picks, vec![vec![bears]], "exactly the Bears are offered");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&alexios].zone, Zone::Battlefield);
    assert_eq!(runner.life(P0), 20, "the sacrifice was paid");
}
