//! Regression: a `TapsForMana` triggered mana ability (Leyline of Abundance —
//! "Whenever you tap a creature for mana, add an additional {G}.") must deliver
//! its bonus mana during an auto-tapped spell cast (CR 605.4a).
//!
//! Bug: the affordability *preview* (`can_cast_object_now`) resolved the
//! trigger and reported the spell castable, but the real cost-payment path
//! (`auto_tap_and_pay_cost_excluding`) did not — so the candidate was generated
//! and then dropped by `SimulationFilter` (which validates via the real
//! `apply`). The spell was silently hidden from `legal_actions` and casting it
//! failed with "Cannot pay mana cost".
//!
//! Fix: the cost-payment path resolves coupled `TapsForMana` triggered mana
//! abilities inline via `resolve_tap_mana_triggers_inline`, and the deferred
//! post-action trigger scan skips what was already resolved (the
//! `ManaTapState::FromTapTriggersResolved` marker) so the bonus fires once.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const LEYLINE_TEXT: &str = "Whenever you tap a creature for mana, add an additional {G}.";

/// `n` units of green mana, as if already floating in a pool.
fn floating_green(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

/// `{3}{G}{G}` — a five-mana cost.
fn cost_3gg() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        generic: 3,
    }
}

#[test]
fn leyline_bonus_unlocks_auto_tapped_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A creature that taps for {G}.
    scenario.add_creature_from_oracle(P0, "Mana Beast", 1, 1, "{T}: Add {G}.");
    // Leyline of Abundance's relevant ability, hosted on a battlefield permanent.
    scenario.add_creature_from_oracle(P0, "Abundance Source", 0, 1, LEYLINE_TEXT);
    // Three green mana already floating in the pool.
    scenario.with_mana_pool(P0, floating_green(3));
    // A five-mana creature in hand: {3}{G}{G}.
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(cost_3gg())
        .id();

    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    // Reachable mana = 3 floating + {G} (tap Mana Beast) + {G} (Leyline bonus) = 5 = cost.
    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == threat_id
        )),
        "Big Threat must be a legal CastSpell action: 3 floating + tapped {{G}} + \
         Leyline bonus {{G}} = 5 reachable mana covers {{3}}{{G}}{{G}}",
    );

    let stack_before = runner.state().stack.len();
    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Big Threat must succeed — Leyline's bonus mana must reach the pool");
    assert_eq!(
        runner.state().stack.len(),
        stack_before + 1,
        "Big Threat should be on the stack after a successful cast",
    );
}

#[test]
fn leyline_bonus_fires_exactly_once_during_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Mana Beast", 1, 1, "{T}: Add {G}.");
    scenario.add_creature_from_oracle(P0, "Abundance Source", 0, 1, LEYLINE_TEXT);
    scenario.with_mana_pool(P0, floating_green(3));
    let threat_id = scenario
        .add_creature_to_hand(P0, "Big Threat", 6, 6)
        .with_mana_cost(cost_3gg())
        .id();

    let mut runner = scenario.build();
    let threat_card = runner.state().objects[&threat_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: threat_id,
            card_id: threat_card,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must succeed");

    // 3 floating + 1 tapped + 1 Leyline bonus = exactly 5, fully spent on
    // {3}{G}{G}. A double-fire (payment-time resolution AND post-action scan)
    // would add a second bonus {G}, leaving one mana floating.
    let leftover = runner.state().players[0].mana_pool.mana.len();
    assert_eq!(
        leftover, 0,
        "Leyline must fire exactly once — the post-action trigger scan must not \
         re-resolve a triggered mana ability already resolved at payment time; \
         found {leftover} mana left floating",
    );
}

#[test]
fn leyline_bonus_still_fires_on_manual_mana_activation() {
    // The cost-payment fix must not regress the manual path: activating a mana
    // ability directly still triggers Leyline via the post-action trigger scan
    // (the tap event stays `FromTapPending` and is resolved there as before).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let beast_id = scenario
        .add_creature_from_oracle(P0, "Mana Beast", 1, 1, "{T}: Add {G}.")
        .id();
    scenario.add_creature_from_oracle(P0, "Abundance Source", 0, 1, LEYLINE_TEXT);

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: beast_id,
            ability_index: 0,
        })
        .expect("activating the mana ability must succeed");

    let pool = runner.state().players[0].mana_pool.mana.len();
    assert_eq!(
        pool, 2,
        "manual mana-ability activation must still receive Leyline's bonus {{G}} \
         via the post-action trigger scan: {{G}} from the tap + {{G}} from Leyline; \
         found {pool} mana",
    );
}
