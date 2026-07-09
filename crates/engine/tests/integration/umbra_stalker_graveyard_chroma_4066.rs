//! Parser-fed runtime regression for #4066 — Umbra Stalker's graveyard-scope
//! chroma characteristic-defining ability.
//!
//! "Umbra Stalker's power and toughness are each equal to the number of black
//! mana symbols in the mana costs of cards in your graveyard." (CR 604.3 CDA +
//! CR 202.1 + CR 404.2). This drives the REAL Oracle text through the full
//! pipeline: `from_oracle_text` → `parse_cda_pt_equality` → `parse_cda_quantity`
//! → the zone-general `QuantityRef::Aggregate` over
//! `ObjectProperty::ManaSymbolCount`, then the layer-7b
//! `ContinuousModification::SetDynamicPower`/`SetDynamicToughness` static, and
//! asserts P/T through `evaluate_layers` — the production CDA/layer path, not a
//! direct quantity unit test.
//!
//! The test is discriminating on three axes that the prior implementation got
//! wrong or left unproven:
//!   1. Zone scoping — a black-costed card on the battlefield must NOT count.
//!   2. Owner scoping — an opponent's graveyard card must NOT count, and a card
//!      you OWN but an opponent last CONTROLLED (LKI controller diverges from
//!      owner, CR 109.4) MUST still count. `Owned { You }` reads ownership and is
//!      LKI-independent; a `controller(You)` filter would miscount it.
//!   3. Dynamic re-evaluation — removing a card from your graveyard tracks P/T
//!      down (a permanent static CDA, recomputed each layer pass).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::remove_from_zone;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const UMBRA_STALKER: &str = "Umbra Stalker's power and toughness are each equal to \
    the number of black mana symbols in the mana costs of cards in your graveyard.";

/// Helper: a printed mana cost from a list of colored shards (generic 0).
fn cost(shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic: 0 }
}

#[test]
fn umbra_stalker_pt_equals_black_symbols_in_your_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Umbra Stalker is printed */* — seed base 0/0 so any nonzero P/T can only
    // come from the dynamic CDA, never the printed value.
    let stalker = scenario
        .add_creature(P0, "Umbra Stalker", 0, 0)
        .from_oracle_text(UMBRA_STALKER)
        .id();

    // P0's graveyard: {B}{B} (2 black) and {B}{U} (1 black) → 3 black symbols.
    let double_black = scenario
        .add_creature_to_graveyard(P0, "Double Black", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Black, ManaCostShard::Black]))
        .id();
    // This card P0 OWNS, but an opponent (P1) last controlled it before it hit
    // P0's graveyard. Its live `controller` is set below to diverge from owner.
    let owned_opp_controlled = scenario
        .add_creature_to_graveyard(P0, "Reclaimed Spell", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Black, ManaCostShard::Blue]))
        .id();

    // Decoys that must NOT count:
    //  - wrong zone: a black-costed permanent on P0's battlefield.
    scenario
        .add_creature(P0, "Battlefield Decoy", 2, 2)
        .with_mana_cost(cost(vec![ManaCostShard::Black, ManaCostShard::Black]));
    //  - wrong owner: a black-costed card in the OPPONENT's graveyard.
    scenario
        .add_creature_to_graveyard(P1, "Opponent Graveyard Card", 1, 1)
        .with_mana_cost(cost(vec![ManaCostShard::Black, ManaCostShard::Black]));

    let mut runner = scenario.build();

    // CR 109.4: diverge the at-departure controller from the owner. P0 owns the
    // card; P1 last controlled it. An owner-scoped filter still counts it for P0;
    // a controller-scoped filter would not.
    runner
        .state_mut()
        .objects
        .get_mut(&owned_opp_controlled)
        .unwrap()
        .controller = P1;

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // 3 black symbols in P0's graveyard ({B}{B} + {B}{U}); the battlefield decoy
    // (wrong zone) and the opponent's graveyard card (wrong owner) are excluded,
    // and the owned-but-opponent-controlled card IS included (owner scoping).
    assert_eq!(
        runner.state().objects[&stalker].power,
        Some(3),
        "power equals black mana symbols in YOUR graveyard (3), not the printed value"
    );
    assert_eq!(
        runner.state().objects[&stalker].toughness,
        Some(3),
        "toughness equals black mana symbols in YOUR graveyard (3)"
    );

    // CR 604.3: the CDA re-evaluates continuously. Remove the {B}{B} card from
    // P0's graveyard → 1 black symbol remains ({B}{U}) → P/T tracks down to 1/1.
    // This is what discriminates the fix: with the quantity left Unimplemented the
    // P/T would stay at the printed 0/0 and never track at all.
    remove_from_zone(runner.state_mut(), double_black, Zone::Graveyard, P0);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&stalker].power,
        Some(1),
        "power tracks down to the remaining black symbol ({{B}}{{U}}) after a card leaves the graveyard"
    );
    assert_eq!(
        runner.state().objects[&stalker].toughness,
        Some(1),
        "toughness tracks down to 1 after a card leaves the graveyard"
    );
}
