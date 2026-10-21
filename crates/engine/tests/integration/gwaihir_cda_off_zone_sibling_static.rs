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
//! The original admission-only fixture remains below. The printed-card runtime
//! regression additionally checks numeric materialization and current-base P/T
//! in every represented zone through ordinary public action boundaries.

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

#[test]
fn cda_printed_numeric_reaches_each_represented_zone() {
    use engine::game::scenario::P1;
    use engine::game::zones::move_to_zone;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    const CRUSADER: &str = "Crusader of Odric's power and toughness are each equal to the number of creatures you control.";
    const DAKKON: &str = "Dakkon Blackblade's power and toughness are each equal to the number of lands you control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..2 {
        scenario.add_creature(P0, "Own population", 2, 2);
        scenario.add_basic_land(P0, ManaColor::White);
    }
    for _ in 0..5 {
        scenario.add_creature(P1, "Foreign population", 2, 2);
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let remote = [Zone::Library, Zone::Hand, Zone::Graveyard, Zone::Exile].map(|zone| {
        let card = scenario
            .add_creature_to_hand_from_oracle(P0, "Crusader of Odric", 0, 0, CRUSADER)
            .with_subtypes(vec!["Human", "Soldier"])
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White],
            })
            .id();
        (card, zone)
    });
    let cast = scenario
        .add_creature_to_hand_from_oracle(P0, "Crusader of Odric", 0, 0, CRUSADER)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let commander = scenario
        .add_creature_from_oracle(P0, "Dakkon Blackblade", 0, 0, DAKKON)
        .as_legendary()
        .with_subtypes(vec!["Human", "Warrior"])
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
        })
        .id();
    scenario.with_commander(commander);
    scenario.with_mana_pool(
        P0,
        [ManaType::White, ManaType::Colorless, ManaType::Colorless]
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    for &(card, zone) in &remote {
        if zone != Zone::Hand {
            move_to_zone(runner.state_mut(), card, zone, &mut vec![]);
        }
    }
    engine::game::layers::flush_layers(runner.state_mut());
    // CR 604.3 + CR 613.4a: all-zone CDAs use the owner's population off-stack.
    for &(card, zone) in &remote {
        let object = &runner.state().objects[&card];
        assert_eq!(object.zone, zone);
        assert_eq!((object.power, object.toughness), (Some(2), Some(2)));
        assert_eq!(
            (object.layer_base_power, object.layer_base_toughness),
            (Some(2), Some(2))
        );
        assert_eq!(
            (object.base_power, object.base_toughness),
            (Some(0), Some(0))
        );
        assert!(active_static_definitions(runner.state(), object)
            .any(|definition| definition.characteristic_defining));
    }
    assert_eq!(
        (
            runner.state().objects[&commander].power,
            runner.state().objects[&commander].toughness
        ),
        (Some(2), Some(2))
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    let committed = runner.cast(cast).commit();
    assert_eq!(committed.state().objects[&cast].zone, Zone::Stack);
    assert_eq!(
        (
            committed.state().objects[&cast].power,
            committed.state().objects[&cast].toughness
        ),
        (Some(2), Some(2))
    );
    assert_eq!(committed.state().players[0].mana_pool.total(), 0);
    let outcome = committed.resolve();
    assert_eq!(outcome.zone_of(cast), Zone::Battlefield);
    assert_eq!(
        (
            outcome.state().objects[&cast].power,
            outcome.state().objects[&cast].toughness
        ),
        (Some(3), Some(3))
    );
    for &(card, _) in &remote {
        assert_eq!(outcome.state().objects[&card].power, Some(3));
    }
    assert_eq!(outcome.state().objects[&commander].power, Some(2));
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
}
