//! Off-zone CDA regression for PR #8229 review (HIGH finding).
//!
//! `for_each_static_effect_source`'s off-zone candidate scan and the
//! `StaticZoneAdmission::LiveSource` gate (`crates/engine/src/game/layers.rs`)
//! both need to treat a characteristic-defining ability (CDA,
//! `characteristic_defining: true`) as a THIRD zone-admission class, distinct
//! from both:
//!
//! - a plain static with empty `active_zones` (battlefield-only default,
//!   CR 113.6), and
//! - an opt-in off-zone static with a non-empty `active_zones` list (e.g. a
//!   cost reducer).
//!
//! CR 604.3: "Characteristic-defining abilities function in all zones."
//! Unlike an opt-in off-zone static, a CDA does not declare `active_zones` at
//! all (see `parser/oracle_static/cda.rs`'s constructors) — its all-zones
//! behavior is intrinsic to the ability class, not an explicit list.
//!
//! This fixture mirrors Gwaihir the Windlord's OWN shape (a card with a
//! broad-`active_zones` cost-reduction static as one definition, and a
//! second definition with empty `active_zones` as a sibling on the SAME
//! object) but swaps the empty-`active_zones` sibling for a CDA P/T line
//! instead of a keyword grant — so the off-zone CDA path is proven reachable
//! specifically THROUGH a source admitted by a sibling's broad zone list,
//! exactly as the maintainer's review requested. Before the fix,
//! `static_functions_in_zone` applied the plain empty-`active_zones`
//! battlefield-only default to the CDA definition too, rejecting it
//! identically to how Gwaihir's vigilance grant was wrongly rejected in
//! #8158 — except a CDA is SUPPOSED to function everywhere, so that was the
//! wrong answer in the opposite direction.
//!
//! Drives the REAL parse -> synthesis -> layer pipeline, like
//! `gwaihir_vigilance_zone_gate.rs`, and observes the result through
//! `functioning_abilities::active_static_definitions` — the same public,
//! single-authority "is this static definition currently functioning on this
//! object" query used pervasively elsewhere in the engine (combat legality,
//! casting checks, etc.), which delegates to the exact `static_functions_in_zone`
//! predicate this PR fixed.
//!
//! Note on scope: this test deliberately does NOT assert that the CDA's
//! `power`/`toughness` VALUES get written back onto the Hand object via
//! `evaluate_layers`. A `SelfRef`-affected continuous effect's recipient scan
//! (`layers::collect_scan_zones`) defaults to `Zone::Battlefield` for any
//! `SelfRef` filter regardless of the source's actual zone — a separate,
//! pre-existing limitation unrelated to (and out of scope for) the
//! admission-layer fix under test here. What this test proves is that the
//! CDA definition is correctly ADMITTED as functioning off-battlefield (the
//! exact thing the maintainer's review flagged); a follow-up issue tracks
//! materializing that admission into observable off-zone characteristics.

use engine::game::functioning_abilities::active_static_definitions;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::GameScenario;
use engine::game::scenario::P0;

const KITE_ORACLE: &str = "This spell costs {1} less to cast as long as you control a Bird.\nTest CDA Kite's power and toughness are each equal to the number of Birds you control.";

#[test]
fn cda_admitted_off_battlefield_via_sibling_broad_zone_static() {
    let mut scenario = GameScenario::new();

    // "Test CDA Kite" sits in HAND. Its cost reducer (broad `active_zones`,
    // same shape as Gwaihir's) is the sibling that admits it into
    // `for_each_static_effect_source`'s off-zone candidate scan; its OTHER
    // definition is the CDA P/T line, which carries NO `active_zones` of its
    // own (per `parser/oracle_static/cda.rs`) and must be admitted on CR
    // 604.3's own "functions in all zones" authority, not rejected as a
    // plain empty-`active_zones` battlefield-only static.
    let kite = scenario
        .add_creature_to_hand_from_oracle(P0, "Test CDA Kite", 0, 0, KITE_ORACLE)
        .with_subtypes(vec!["Bird"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let state = runner.state();
    let obj = &state.objects[&kite];
    assert_eq!(obj.zone, engine::types::zones::Zone::Hand);

    let cda_is_functioning = active_static_definitions(state, obj)
        .any(|def| def.characteristic_defining && def.active_zones.is_empty());

    assert!(
        cda_is_functioning,
        "CR 604.3: a characteristic-defining ability functions in all zones — \
         Test CDA Kite's P/T-setting CDA (empty `active_zones`, \
         `characteristic_defining: true`) must be admitted as functioning \
         while the source sits in Hand, reached via its own CR 604.3 \
         authority rather than rejected under the plain empty-`active_zones` \
         battlefield-only default that correctly still applies to ordinary \
         (non-CDA) statics"
    );
}
