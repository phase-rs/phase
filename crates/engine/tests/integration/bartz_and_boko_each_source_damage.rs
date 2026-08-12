//! Runtime discriminator: the filter-source own-power damage class — "each other
//! <type> you control deals damage equal to its power to <recipient>" — parsed to
//! `Effect::EachSourceDealsDamage` with a per-batch-source `Power { BatchSource }`
//! amount, resolved PER SOURCE (each member is the source of its own damage,
//! CR 120.1), NOT resolved once against the ability source.
//!
//! Card under test: Bartz and Boko's ETB trigger:
//!   "When Bartz and Boko enters, each other Bird you control deals damage equal
//!    to its power to target creature an opponent controls."
//!
//! The board is sized so the assertions DISCRIMINATE every failure mode:
//!   - Correct per-source resolution: 4/4 + 5/5 = 9 damage ≥ 9 toughness kills the
//!     opponent's 0/9 target (CR 120.6 damage marking; CR 704.5g lethal-damage
//!     state-based action).
//!   - A resolver reading the amount once against the ability source (Bartz, 4)
//!     and applying it per source deals 4 + 4 = 8 < 9 → target survives.
//!   - A resolver reading once against the FIRST batch member (the 4/4, placed
//!     first so the batch iterates 4/4 → 5/5) deals 4 + 4 = 8 < 9 → survives.
//!   - The pre-fix parser misparse (single `DealDamage` at Bartz's own 4 once)
//!     deals 4 < 9 → survives.
//!   - A fail-closed resolver (`damage_source` unset → 0) deals 0 → survives.
//!
//! The correct arm is the ONLY one that kills the 0/9.
//!
//! `objects_that_dealt_damage` is keyed per source (deal_damage.rs), so the
//! attribution assertion (BOTH Birds marked, Bartz NOT) pairs with — but does not
//! substitute for — the creature-dies threshold (a uniform-amount resolver still
//! keys per source and would satisfy attribution alone).
//!
//! CR 120.1: the object that deals damage is the source of that damage.
//! CR 120.6: lethal damage is total marked damage ≥ toughness (definition).
//! CR 704.5g: a creature with lethal damage marked on it is destroyed (SBA).
//! CR 113.7a: LKI fallback for sources that leave the battlefield mid-batch.
//! CR 208.1 + CR 608.2: a creature's power is a modifiable characteristic read at
//!            resolution.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall) — Bartz and Boko, {2}{U}{R} Legendary Creature —
/// Human Bird. Affinity for Birds reduces the cost; the cast below pays a zeroed
/// cost so the affinity line is irrelevant to the mechanic under test.
const BARTZ_AND_BOKO: &str = "Affinity for Birds (This spell costs {1} less to cast for each \
     Bird you control.)\nWhen Bartz and Boko enters, each other Bird you control deals damage \
     equal to its power to target creature an opponent controls.";

/// Place a creature on the battlefield under `player` that matches "each other
/// Bird you control" (a Bird subtype is the whole filter — the parser emitted
/// `TypeFilter::Subtype("Bird")` without a redundant Creature type).
fn add_bird(
    scenario: &mut GameScenario,
    player: engine::types::PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature(player, name, power, toughness)
        .with_subtypes(vec!["Bird"])
        .id()
}

/// The 4/4 enters on the battlefield FIRST (before Bartz is cast), so a
/// resolve-once-against-the-first-batch-member bug reads the 4/4 → 4 + 4 = 8 < 9
/// (survives). The correct per-source read 4 + 5 = 9 kills the 0/9.
#[test]
fn bartz_each_other_bird_deals_own_power_sum_kills_0_9() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // 4/4 on the battlefield first, then the 5/5 (battlefield is insertion-ordered).
    let bird4 = add_bird(&mut scenario, P0, "Gwaihir the Windlord", 4, 4);
    let bird5 = add_bird(&mut scenario, P0, "Dragonhawk", 5, 5);
    // The opponent's target: 9 toughness survives 8 (any wrong arm) but dies to 9.
    let recipient = scenario.add_vanilla(P1, 0, 9);

    let bartz = scenario
        .add_creature_to_hand_from_oracle(P0, "Bartz and Boko", 4, 3, BARTZ_AND_BOKO)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // Cast Bartz; the harness drives the ETB trigger's TriggerTargetSelection from
    // the declared `recipient` intent (CR 603.3d).
    let outcome = runner.cast(bartz).target_object(recipient).resolve();
    let state = outcome.state();

    // CR 120.1 + CR 208.1 + CR 608.2: each of the two OTHER Birds deals its own
    // power — 4 + 5 = 9 marked. 9 ≥ 9 toughness is lethal under CR 120.6, so the
    // 0/9 is destroyed via the CR 704.5g state-based action.
    assert_eq!(
        outcome.zone_of(recipient),
        Zone::Graveyard,
        "0/9 must be dealt 4 + 5 = 9 (own-power per source) and die; \
         uniform-against-Bartz 8, first-source 8, pre-fix single 4, and fail-closed 0 \
         all leave it alive — got {:?}",
        outcome.zone_of(recipient)
    );

    // Per-source attribution (CR 120.1): BOTH Birds are recorded as damage
    // sources; Bartz (excluded by "other" — he is himself a Bird per the Human
    // Bird power 4 typeline) contributes 0 and must NOT be in the dealt set.
    assert!(
        state.objects_that_dealt_damage.contains(&bird4),
        "the 4/4 Bird must be a recorded damage source"
    );
    assert!(
        state.objects_that_dealt_damage.contains(&bird5),
        "the 5/5 Bird must be a recorded damage source"
    );
    assert!(
        !state.objects_that_dealt_damage.contains(&bartz),
        "Bartz must NOT deal damage — the 'other Bird' exclusion binds per source; \
         got dealt set {:?}",
        state.objects_that_dealt_damage
    );
}

/// Negative paired with the positive above (reach-guard): with NO other Birds on
/// the battlefield, the "each OTHER Bird you control" filter has zero members, so
/// the trigger deals 0 to the 0/9 (it survives) and `objects_that_dealt_damage` is
/// empty (Bartz — himself a Bird — is excluded). This proves the `FilterProp::Another`
/// exclusion and the zero-member per-source skip are load-bearing, not vacuous:
/// the positive two-Bird test would fail if the "other" filter were dropped
/// (Bartz would wrongly deal 4 and appear in the dealt set).
#[test]
fn bartz_no_other_bird_deals_zero_target_survives() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Only Bartz — he is a Human Bird, but "other" excludes him from his own filter.
    let bartz = scenario
        .add_creature_to_hand_from_oracle(P0, "Bartz and Boko", 4, 3, BARTZ_AND_BOKO)
        .with_mana_cost(ManaCost::zero())
        .id();
    let recipient = scenario.add_vanilla(P1, 0, 9);

    let mut runner = scenario.build();
    let outcome = runner.cast(bartz).target_object(recipient).resolve();
    let state = outcome.state();

    assert_eq!(
        outcome.zone_of(recipient),
        Zone::Battlefield,
        "with no other Birds, the trigger must deal 0 and the 0/9 survives"
    );
    assert!(
        state.objects_that_dealt_damage.is_empty(),
        "no Bird source dealt damage (Bartz excluded as 'other'), got {:?}",
        state.objects_that_dealt_damage
    );
}
