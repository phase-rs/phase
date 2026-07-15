//! Regression for issue #4955: Greenbelt Rampager's ETB —
//! "When this creature enters, pay {E}{E} (two energy counters). If you
//! can't, return this creature to its owner's hand and you get {E}." — always
//! bounced the creature back to its owner's hand, even when the controller
//! had 2+ energy and actually paid the cost.
//!
//! Root cause (CR 608.2c + CR 118.1 + CR 118.3): the generic "if you can't"
//! rider lowers to `AbilityCondition::Not { ZoneChangedThisWay { Any } }` — a
//! proxy that reads `state.last_zone_changed_ids`, which is populated only by
//! effects that move an object between zones (search/exile/sacrifice/…).
//! `Effect::PayCost` (the "pay {E}{E}" instruction) deducts energy from the
//! player's pool and moves no object anywhere, so the zone-change ledger
//! stayed empty regardless of whether the payment actually succeeded —
//! `Not { ZoneChangedThisWay { Any } }` was therefore true UNCONDITIONALLY,
//! firing the bounce rider every time regardless of affordability.
//!
//! Fix: `rewrite_cant_rider_for_non_zone_change_parent`
//! (`crates/engine/src/parser/oracle_effect/mod.rs`) — already carved out for
//! the analogous `Effect::TurnFaceUp` class (Etrata, Deadly Fugitive: "Turn
//! this creature face up. If you can't, exile it …") — now also recognizes
//! `Effect::PayCost` as a zone-change-ledger-invisible parent and rewrites the
//! rider to `Not { OptionalEffectPerformed }`. That signal is fed by the
//! resolution-time cost-payment authority's `cost_payment_failed_flag`
//! (`game::effects::pay::resolve` / `resolve_ability_cost_payment`), which
//! correctly distinguishes a paid {E}{E} from an unpaid one — and, via the
//! mandatory-rider seed in `resolve_ability_chain`
//! (`game::effects::mod::mandatory_parent_effect_performed` falls through to
//! its `_ => true` default for `PayCost`, so the seed reduces to exactly
//! `!cost_payment_failed_flag`), sets `optional_effect_performed` iff the
//! payment succeeded.
//!
//! Both tests drive the real casting + ETB-trigger pipeline against the
//! verified Oracle text (Scryfall, 2026-07-14): {G} Elephant, 3/4.

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GREENBELT_RAMPAGER_ORACLE: &str = "When this creature enters, pay {E}{E} (two energy \
     counters). If you can't, return this creature to its owner's hand and you get {E}.";

/// Build a scenario with Greenbelt Rampager in P0's hand, funded for its real
/// {G} cost, with P0's starting energy set to `starting_energy`.
fn scenario_with_rampager(starting_energy: u32) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rampager = scenario
        .add_creature_to_hand_from_oracle(P0, "Greenbelt Rampager", 3, 4, GREENBELT_RAMPAGER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Green,
            ObjectId(9_999),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .energy = starting_energy;

    (runner, rampager)
}

fn energy_of(state: &engine::types::game_state::GameState) -> u32 {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .energy
}

/// Has enough energy ({E}{E} = 2) and pays it: the creature STAYS on the
/// battlefield, the two energy are spent, and the bounce rider's "you get
/// {E}" must NOT also fire on the paid branch (net energy: 2 -> 0, not 3).
#[test]
fn greenbelt_rampager_pays_energy_and_stays_on_battlefield() {
    let (mut runner, rampager) = scenario_with_rampager(2);

    let outcome = runner.cast(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Battlefield);
    let energy = energy_of(outcome.state());
    assert_eq!(
        energy, 0,
        "paying {{E}}{{E}} must deduct both energy and must NOT also grant the \
         bounce rider's {{E}}; energy={energy}"
    );
}

/// Doesn't have enough energy (0 available, needs 2) and can't pay: the
/// creature BOUNCES to its owner's hand and the controller gets {E} (net
/// energy: 0 -> 1). This is the issue #4955 regression assertion in reverse —
/// before the fix, this branch was the ONLY branch, even when energy was
/// available (see the paid-branch test above).
#[test]
fn greenbelt_rampager_cant_pay_energy_bounces_to_hand() {
    let (mut runner, rampager) = scenario_with_rampager(0);

    let outcome = runner.cast(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Hand);
    let energy = energy_of(outcome.state());
    assert_eq!(
        energy, 1,
        "an unpayable {{E}}{{E}} must bounce the creature to hand and grant {{E}} \
         exactly once; energy={energy}"
    );
}
