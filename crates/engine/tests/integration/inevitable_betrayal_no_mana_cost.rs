//! Regression for issue #827: Inevitable Betrayal (Suspend 3—{1}{U}{U}, no mana
//! cost) could be cast "normally" for free from hand.
//!
//! CR 202.1b: a card with no mana symbols where its mana cost would appear has
//! no mana cost. CR 118.6: no mana cost is an unpayable cost, so the card can't
//! be cast from hand by paying it — its only outlet is the Suspend activation.
//! CR 118.6a: an effect that lets you cast it *without paying its mana cost*
//! (e.g. Omniscience) is the exception and may still cast it.
//!
//! Two coupled defects produced the original bug:
//!   - the card-data pipeline emitted `Cost{0}` instead of `ManaCost::NoCost`
//!     for an absent MTGJSON `manaCost` (synthesis `.unwrap_or_default()`), and
//!   - the hand-cast gate (`can_cast_object_now` / `handle_cast_spell`) never
//!     rejected no-mana-cost cards.

use engine::ai_support::legal_actions;
use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;
use engine::types::game_state::CastPaymentMode;

/// Core regression: a no-mana-cost card can't be cast normally from hand, a real
/// {0} card still can, and the no-mana-cost card is NOT bricked — its Suspend
/// activation remains available.
#[test]
fn inevitable_betrayal_no_mana_cost_blocks_normal_cast_but_keeps_suspend() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let betrayal = scenario.add_real_card(P0, "Inevitable Betrayal", Zone::Hand, db);
    // Control: a real {0} card (explicit "{0}" manaCost) must stay castable —
    // this is what distinguishes NoCost from a payable Cost{generic:0}.
    let ornithopter = scenario.add_real_card(P0, "Ornithopter", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // {1}{U}{U} so the Suspend activation (its cost) is affordable and offerable.
    {
        let pool = &mut runner.state_mut().players[0].mana_pool;
        pool.add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
        pool.add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    // Pipeline fix: no printed mana cost -> NoCost, not a payable {0} cost.
    assert!(
        matches!(
            runner.state().objects[&betrayal].mana_cost,
            ManaCost::NoCost
        ),
        "Inevitable Betrayal must parse with ManaCost::NoCost, got {:?}",
        runner.state().objects[&betrayal].mana_cost
    );

    // CR 202.1b + 118.6: an unpayable (no-mana) cost can't be cast normally.
    // (no mana cost => unpayable => not castable by paying it)
    assert!(
        !can_cast_object_now(runner.state(), P0, betrayal),
        "Inevitable Betrayal (no mana cost) must NOT be castable normally from hand (CR 118.6)"
    );

    // Control: the real {0} artifact stays castable — the NoCost gate must not
    // bleed into payable {0} costs.
    assert!(
        can_cast_object_now(runner.state(), P0, ornithopter),
        "A real {{0}} card (Ornithopter) must remain castable from hand"
    );

    // Not bricked: the card is still playable via its Suspend activation
    // (a separate ActivateAbility from hand). A future over-broadening of the
    // NoCost gate that also suppressed this would be caught here.
    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == betrayal
        )),
        "Inevitable Betrayal must still offer its Suspend activation from hand"
    );

    // Defense-in-depth: a direct CastSpell action is rejected even if it
    // bypasses the candidate generator (stale action / hand-crafted payload).
    let card_id = runner.state().objects[&betrayal].card_id;
    let res = runner.act(GameAction::CastSpell {
        object_id: betrayal,
        card_id,
        targets: vec![],

        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        res.is_err(),
        "casting a no-mana-cost card from hand must be InvalidAction, got {res:?}"
    );
}

/// CR 118.6a: an effect that lets you cast a card *without paying its mana cost*
/// (Omniscience — `CastFromHandFree` with `Unlimited` frequency) overrides the
/// unpayable-cost block, so even a no-mana-cost card becomes castable from hand.
/// Guards against the gate over-blocking the Omniscience class.
#[test]
fn no_mana_cost_card_is_castable_from_hand_under_omniscience() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let betrayal = scenario.add_real_card(P0, "Inevitable Betrayal", Zone::Hand, db);
    scenario.add_real_card(P0, "Omniscience", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert!(
        can_cast_object_now(runner.state(), P0, betrayal),
        "With Omniscience (Unlimited CastFromHandFree) in play, a no-mana-cost card \
         must be castable from hand without paying its mana cost (CR 118.6a)"
    );
}
