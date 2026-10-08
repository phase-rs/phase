//! Dragon Man, Reformed Robot — graveyard cast permission with an ADDITIONAL
//! discard cost (CR 601.2f + CR 701.9a).
//!
//! "You may cast this card from your graveyard by discarding a card in addition
//! to paying its other costs."
//!
//! The permission keeps the spell's printed mana cost (CR 601.2f: an ADDITIONAL
//! cost, not an alternative one) and requires discarding a card on top. Unlike
//! Festival of Embers' pay-life cost, the discard is INTERACTIVE — the discarded
//! card must be declared via `.pay_cost_with(..)` (a bare `.resolve()` submits an
//! empty selection, which the engine rejects). These tests drive the real cast
//! pipeline and prove (a) the discard is actually paid, (b) the cast is illegal
//! with no discardable card, and (c) the discard is mandatory.
//!
//! Dragon Man is modeled here as a Legendary Creature (the Artifact core type is
//! not load-bearing for the discard-cost behavior under test); its verbatim
//! Oracle text drives the parser as it would in production.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRAGON_MAN_ORACLE: &str = "Flying\n\
Dragon Man's power is equal to the greatest mana value among noncreature permanents you control and noncreature cards in your graveyard.\n\
You may cast this card from your graveyard by discarding a card in addition to paying its other costs.";

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = engine::types::identifiers::ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

/// {2}{W}{U} — Dragon Man's printed mana cost, still due as an ADDITIONAL rider.
fn dragon_man_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White, ManaCostShard::Blue],
        generic: 2,
    }
}

/// Pool that covers {2}{W}{U}: W, U, and two colorless for the generic {2}.
fn full_pool() -> Vec<ManaUnit> {
    pool_units(&[
        ManaType::White,
        ManaType::Blue,
        ManaType::Colorless,
        ManaType::Colorless,
    ])
}

fn stage_dragon_man(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Dragon Man, Reformed Robot", 0, 5)
        .as_legendary()
        .with_subtypes(vec!["Dragon", "Robot"])
        .with_mana_cost(dragon_man_cost())
        .from_oracle_text(DRAGON_MAN_ORACLE)
        .id()
}

/// CR 601.2f + CR 701.9a: end-to-end — casting Dragon Man from the graveyard pays
/// its {2}{W}{U} AND discards a card. DISCRIMINATING: reverting the parser so the
/// permission's `extra_cost` is `None` removes the discard requirement, so the
/// declared hand card would stay in hand (`Zone::Hand`) instead of moving to the
/// graveyard — the assertion flips.
#[test]
fn dragon_man_graveyard_cast_requires_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let dragon_man_id = stage_dragon_man(&mut scenario);
    let hand_card = scenario.add_card_to_hand(P0, "Forest");
    scenario.with_mana_pool(P0, full_pool());
    let mut runner = scenario.build();

    // The discard is interactive — declare which card pays it (CR 601.2b).
    let outcome = runner
        .cast(dragon_man_id)
        .pay_cost_with(&[hand_card])
        .resolve();

    assert_eq!(
        outcome.zone_of(hand_card),
        Zone::Graveyard,
        "the declared discard must actually be paid — the hand card moves to the graveyard"
    );
    assert_eq!(
        outcome.zone_of(dragon_man_id),
        Zone::Battlefield,
        "Dragon Man resolves onto the battlefield after its additional discard cost is paid"
    );
}

/// CR 601.2h + CR 118.3: an unpayable additional cost makes the cast illegal.
/// With P0's hand empty, there is no card to discard, so legal actions must NOT
/// offer the graveyard cast. DISCRIMINATING: reverting the legality gate to the
/// pay-life-only `find_pay_life_cost` check makes it a no-op for `Discard`, so it
/// returns `true` and this assertion fails. The paired positive
/// (`dragon_man_graveyard_cast_requires_discard`, same mana pool) proves the
/// block is affordability-specific, not a blanket refusal.
#[test]
fn dragon_man_graveyard_cast_blocked_without_discardable_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let dragon_man_id = stage_dragon_man(&mut scenario);
    // P0's hand is empty — nothing to discard for the mandatory additional cost.
    scenario.with_mana_pool(P0, full_pool());
    let runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, dragon_man_id),
        "with no discardable card the mandatory additional discard cost (CR 601.2h) \
         is unpayable, so the graveyard cast must not be offered"
    );
}

/// CR 601.2f: the additional discard is MANDATORY. A card IS available to discard
/// (so the cost is payable), but the caster declines to declare one — the driver
/// then submits an empty discard selection. Discarding nothing cannot satisfy the
/// count-1 cost, so the engine rejects the cast. This proves the `min_count: 0`
/// lower bound on the `PayCost` window does not permit paying nothing on this
/// path (enforcement is in `handle_discard_for_cost`, which requires
/// `chosen.len() == count`). DISCRIMINATING: reverting the parser removes the
/// discard window entirely, so the cast succeeds and `is_err()` fails.
#[test]
fn dragon_man_graveyard_discard_is_mandatory() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let dragon_man_id = stage_dragon_man(&mut scenario);
    // A discardable card exists, so the cost is payable — but we do NOT declare it.
    let _hand_card = scenario.add_card_to_hand(P0, "Forest");
    scenario.with_mana_pool(P0, full_pool());
    let mut runner = scenario.build();

    // No `.pay_cost_with(..)` — the driver submits an empty discard selection.
    let result = runner.cast(dragon_man_id).try_resolve();

    assert!(
        result.is_err(),
        "casting Dragon Man from the graveyard without discarding a card must be \
         rejected — the additional discard cost is mandatory"
    );
}
