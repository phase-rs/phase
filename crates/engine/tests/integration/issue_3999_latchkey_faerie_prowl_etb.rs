//! Issue #3999 — Latchkey Faerie's ETB draw is gated on "if its prowl cost was
//! paid", but the intervening-if was dropped: it drew unconditionally.
//!
//! Oracle: "Flying\nProwl {2}{U}\nWhen this creature enters, if its prowl cost
//! was paid, draw a card."
//!
//! Two coupled fixes are exercised here, end-to-end through the real cast
//! pipeline (no manual tagging):
//!   1. The parser lowers "if its prowl cost was paid" to
//!      `TriggerCondition::CastVariantPaid { Prowl }` (CR 702.76a + CR 603.4).
//!   2. Prowl is wired into the normal-vs-alternative cast flow
//!      (`AlternativeCastKeyword::Prowl`), so a single-Prowl card can actually be
//!      cast for its prowl cost; `stack.rs` tags `cast_variant_paid = Prowl` at
//!      resolution, which the intervening-if reads.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{CastVariantPaid, TriggerCondition};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const LATCHKEY: &str =
    "Flying\nProwl {2}{U}\nWhen this creature enters, if its prowl cost was paid, draw a card.";

fn mana_pool(generic: usize, blue: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..blue {
        pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
    pool
}

/// Cast Latchkey from hand through the real pipeline — via its prowl cost when
/// `prowl` is set — and return how many cards the controller drew (library
/// shrinkage) once its ETB has resolved.
fn library_drawn_via_cast(prowl: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let latchkey = scenario
        .add_creature_to_hand_from_oracle(P0, "Latchkey Faerie", 2, 2, LATCHKEY)
        .with_subtypes(vec!["Faerie", "Rogue"])
        // Printed cost {3}{U}: the prowl alternative ({2}{U}) is then a genuine
        // distinct choice rather than a free hard-cast.
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    scenario.add_card_to_library_top(P0, "Island");
    scenario.add_card_to_library_top(P0, "Mountain");
    // Prowl case: exactly {2}{U} (3 mana) so the printed {3}{U} is UNaffordable
    // and the only payable option is prowl — the engine auto-routes to the
    // prowl alternative cast (a real cast through the new normal-vs-prowl path),
    // tagging cast_variant_paid = Prowl at resolution. Hard-cast case: {3}{U} is
    // affordable and no prowl eligibility is seeded → a normal cast.
    let pool = if prowl {
        mana_pool(2, 1)
    } else {
        mana_pool(3, 1)
    };
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    if prowl {
        // CR 702.76a: prowl is legal because a Faerie the caster controlled dealt
        // combat damage to a player this turn (the per-turn creature-type ledger).
        runner
            .state_mut()
            .creature_types_dealt_combat_damage_this_turn
            .insert((P0, "Faerie".to_string()));
    }
    let library_before = runner.state().players[P0.0 as usize].library.len();

    runner.cast(latchkey).resolve();

    library_before.saturating_sub(runner.state().players[P0.0 as usize].library.len())
}

#[test]
fn latchkey_etb_is_gated_on_prowl_cost_paid() {
    // CR 702.76a: the ETB intervening-if must lower to CastVariantPaid { Prowl },
    // not be dropped (which left the draw unconditional — the reported bug).
    let parsed = parse_oracle_text(
        LATCHKEY,
        "Latchkey Faerie",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.condition.is_some())
        .expect("ETB trigger must carry an intervening-if condition");
    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Prowl
        }),
        "\"if its prowl cost was paid\" must gate the ETB, got {:?}",
        trigger.condition
    );
}

#[test]
fn latchkey_draws_when_cast_for_its_prowl_cost() {
    assert_eq!(
        library_drawn_via_cast(true),
        1,
        "Latchkey must draw a card when actually cast for its prowl cost"
    );
}

#[test]
fn latchkey_does_not_draw_when_hard_cast() {
    // The reported bug: it drew irrespective of whether prowl was paid.
    assert_eq!(
        library_drawn_via_cast(false),
        0,
        "Latchkey must NOT draw when it was not cast for its prowl cost"
    );
}
