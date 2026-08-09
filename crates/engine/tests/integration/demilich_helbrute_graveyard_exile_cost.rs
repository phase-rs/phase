//! Demilich & Helbrute — graveyard cast permission with an ADDITIONAL exile cost
//! (CR 601.2f + CR 701.13a).
//!
//! Demilich: "You may cast this card from your graveyard by exiling four instant
//! and/or sorcery cards from your graveyard in addition to paying its other
//! costs."
//! Helbrute: "Sarcophagus — You may cast this card from your graveyard by exiling
//! another creature card from your graveyard in addition to paying its other
//! costs."
//!
//! These are the exile-cost siblings of Dragon Man's discard rider. The regression
//! under guard: when the CR 601.2f additional-cost rider learned to DECLINE on an
//! unmodeled gerund, `parse_gerund_cost` had no `exiling` arm, so both riders
//! lowered to `Unimplemented` and the whole permission was declined — turning both
//! cards from castable-from-graveyard (with the exile cost silently dropped) into
//! entirely uncastable-from-graveyard, masked by green coverage. Adding the
//! `exiling` arm both restores castability AND models the real exile cost.
//!
//! These tests drive the real cast pipeline: the exile is INTERACTIVE — the exiled
//! cards are declared via `.pay_cost_with(..)` at the announcement-time `PayCost`
//! window. Reverting the parser fix (dropping the `exiling` arm) declines the
//! permission, so `can_cast_object_now` returns false and `.resolve()` cannot
//! commit the cast — every assertion below flips.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DEMILICH_ORACLE: &str = "This spell costs {U} less to cast for each instant and sorcery spell you've cast this turn.\n\
You may cast this card from your graveyard by exiling four instant and/or sorcery cards from your graveyard in addition to paying its other costs.";

const HELBRUTE_ORACLE: &str = "Haste\n\
Sarcophagus — You may cast this card from your graveyard by exiling another creature card from your graveyard in addition to paying its other costs.";

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

/// {U}{U}{U}{U} — Demilich's printed mana cost, still due as an ADDITIONAL rider.
fn demilich_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::Blue,
            ManaCostShard::Blue,
            ManaCostShard::Blue,
            ManaCostShard::Blue,
        ],
        generic: 0,
    }
}

/// {3}{B}{R} — Helbrute's printed mana cost.
fn helbrute_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Red],
        generic: 3,
    }
}

fn stage_demilich(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Demilich", 4, 4)
        .with_subtypes(vec!["Skeleton", "Wizard"])
        .with_mana_cost(demilich_cost())
        .from_oracle_text(DEMILICH_ORACLE)
        .id()
}

/// Stage four instant/sorcery cards (2 + 2) in P0's graveyard, all eligible for
/// the "instant and/or sorcery" exile filter.
fn stage_four_spells(scenario: &mut GameScenario) -> Vec<ObjectId> {
    vec![
        scenario
            .add_spell_to_graveyard(P0, "Lightning Bolt", true)
            .id(),
        scenario.add_spell_to_graveyard(P0, "Opt", true).id(),
        scenario
            .add_spell_to_graveyard(P0, "Divination", false)
            .id(),
        scenario.add_spell_to_graveyard(P0, "Ponder", false).id(),
    ]
}

/// CR 601.2f + CR 701.13a: end-to-end — casting Demilich from the graveyard pays
/// its {U}{U}{U}{U} AND exiles four instant/sorcery cards from the graveyard.
/// DISCRIMINATING: reverting the parser (no `exiling` arm) makes the rider
/// `Unimplemented`, so the permission is declined and Demilich is never offered as
/// a cast — the cast cannot be committed and Demilich stays in the graveyard, so
/// both the battlefield assertion and the four exile assertions flip.
#[test]
fn demilich_graveyard_cast_exiles_four_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let demilich_id = stage_demilich(&mut scenario);
    let spells = stage_four_spells(&mut scenario);
    // Pool covers {U}{U}{U}{U}.
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Blue; 4]));
    let mut runner = scenario.build();

    let outcome = runner.cast(demilich_id).pay_cost_with(&spells).resolve();

    assert_eq!(
        outcome.zone_of(demilich_id),
        Zone::Battlefield,
        "Demilich resolves onto the battlefield after its additional exile cost is paid"
    );
    for &spell in &spells {
        assert_eq!(
            outcome.zone_of(spell),
            Zone::Exile,
            "each declared instant/sorcery card must actually be exiled to pay the cost"
        );
    }
}

/// CR 601.2h: the additional exile is affordability-gated. With only three
/// exilable instant/sorcery cards, the mandatory "exile four" cost is unpayable,
/// so the graveyard cast must not be offered. Paired in-test positive reach-guard:
/// with a fourth eligible card (same mana pool) the cast IS offered — proving the
/// block is affordability-specific, not a blanket refusal.
/// DISCRIMINATING: reverting the parser declines the permission unconditionally,
/// so the four-card positive reach-guard also returns false and fails.
#[test]
fn demilich_graveyard_cast_blocked_without_four_exilable_cards() {
    // Only three eligible cards — the "exile four" cost is unpayable.
    let mut blocked = GameScenario::new();
    blocked.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let demilich_blocked = stage_demilich(&mut blocked);
    blocked.add_spell_to_graveyard(P0, "Lightning Bolt", true);
    blocked.add_spell_to_graveyard(P0, "Opt", true);
    blocked.add_spell_to_graveyard(P0, "Divination", false);
    blocked.with_mana_pool(P0, pool_units(&[ManaType::Blue; 4]));
    let blocked_runner = blocked.build();
    assert!(
        !can_cast_object_now(blocked_runner.state(), P0, demilich_blocked),
        "with only three exilable cards the mandatory exile-four additional cost \
         (CR 601.2h) is unpayable, so the graveyard cast must not be offered"
    );

    // Positive reach-guard: a fourth eligible card makes the exact same cast legal.
    let mut allowed = GameScenario::new();
    allowed.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let demilich_allowed = stage_demilich(&mut allowed);
    stage_four_spells(&mut allowed);
    allowed.with_mana_pool(P0, pool_units(&[ManaType::Blue; 4]));
    let allowed_runner = allowed.build();
    assert!(
        can_cast_object_now(allowed_runner.state(), P0, demilich_allowed),
        "with four exilable cards the additional exile cost is payable, so the \
         graveyard cast must be offered — proves the block is affordability-specific"
    );

    // A non-spell graveyard card cannot inflate the filter-specific exile count.
    let mut ineligible = GameScenario::new();
    ineligible.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let demilich_ineligible = stage_demilich(&mut ineligible);
    ineligible.add_spell_to_graveyard(P0, "Lightning Bolt", true);
    ineligible.add_spell_to_graveyard(P0, "Opt", true);
    ineligible.add_spell_to_graveyard(P0, "Divination", false);
    ineligible.add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2);
    ineligible.with_mana_pool(P0, pool_units(&[ManaType::Blue; 4]));
    let ineligible_runner = ineligible.build();
    assert!(
        !can_cast_object_now(ineligible_runner.state(), P0, demilich_ineligible),
        "three instant/sorcery cards plus an ineligible creature cannot pay Demilich's exile-four cost"
    );
}

/// CR 601.2f + CR 701.13a: end-to-end — casting Helbrute from the graveyard pays
/// its {3}{B}{R} AND exiles another creature card from the graveyard. The
/// ability-word prefix ("Sarcophagus —") and Haste keyword ride the same card.
/// DISCRIMINATING: reverting the parser declines the permission, so Helbrute is
/// never offered as a cast and stays in the graveyard — both assertions flip.
#[test]
fn helbrute_graveyard_cast_exiles_another_creature_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let helbrute_id = scenario
        .add_creature_to_graveyard(P0, "Helbrute", 6, 6)
        .with_mana_cost(helbrute_cost())
        .from_oracle_text(HELBRUTE_ORACLE)
        .id();
    // A second creature card in the graveyard satisfies "another creature card".
    let fodder = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    scenario.with_mana_pool(
        P0,
        pool_units(&[
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(helbrute_id).pay_cost_with(&[fodder]).resolve();

    assert_eq!(
        outcome.zone_of(helbrute_id),
        Zone::Battlefield,
        "Helbrute resolves onto the battlefield after its additional exile cost is paid"
    );
    assert_eq!(
        outcome.zone_of(fodder),
        Zone::Exile,
        "the declared other creature card must actually be exiled to pay the cost"
    );
}
