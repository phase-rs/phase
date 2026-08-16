//! Typhoon (Legends) — "Typhoon deals damage to each opponent equal to the
//! number of Islands that player controls."
//!
//! Misparse-backlog category #9 ("Wrong player/controller scope"): the concern
//! is whether each opponent takes damage equal to THEIR OWN Island count, or a
//! single shared/aggregate count. This test is the proof artifact for the
//! "already correct" scope decision — Typhoon's existing `ControllerRef::
//! ScopedPlayer`-based AST is fully correct and NO code change was made for it.
//!
//! `Effect::DamageEachPlayer`'s resolver (`deal_damage::resolve_each_player`)
//! rebinds `scoped_player` per opponent iteration and resolves the amount via
//! `resolve_quantity_scoped`, so each opponent's `ObjectCount { Islands }` is
//! evaluated against that opponent's own battlefield (CR 120.3 + CR 109.5).
//!
//! Two layers of proof:
//!   - runtime: P1 (2 Islands) takes exactly 2, P2 (5 Islands) takes exactly 5 —
//!     a shared/aggregate count would give both the same number.
//!   - parser regression guard: Typhoon's amount is an `ObjectCount` whose filter
//!     controller is `ScopedPlayer` (never silently regressing to `You`/aggregate).

use engine::game::scenario::GameScenario;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ControllerRef, Effect, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (data/card-data.json).
const TYPHOON: &str =
    "Typhoon deals damage to each opponent equal to the number of Islands that player controls.";

/// Runtime discrimination: in a 3-player game each opponent must take damage
/// equal to THEIR OWN Island count. A shared/aggregate scope would deal both
/// opponents the same (e.g. 7, or P0's own count) — the differing 2-vs-5 deltas
/// prove the per-iteration `scoped_player` binding.
#[test]
fn typhoon_damages_each_opponent_by_their_own_island_count() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent Island counts differ so a shared count cannot coincide with both.
    for _ in 0..2 {
        scenario.add_basic_land(P1, ManaColor::Blue); // Island
    }
    for _ in 0..5 {
        scenario.add_basic_land(P2, ManaColor::Blue); // Island
    }
    // P0 controls Islands too — proves the count is NOT the caster's own Islands
    // (P0 has 3; neither opponent takes 3).
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }

    let typhoon = scenario
        .add_spell_to_hand_from_oracle(P0, "Typhoon", false, TYPHOON)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(typhoon).resolve();

    // CR 120.3 + CR 109.5: each opponent takes exactly their own Island count.
    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "P1 controls 2 Islands and must take exactly 2 damage"
    );
    assert_eq!(
        outcome.life_delta(P2),
        -5,
        "P2 controls 5 Islands and must take exactly 5 damage (per-opponent scoped \
         count, not shared/aggregate)"
    );
    // The caster takes no damage (DamageEachPlayer scopes to opponents only).
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "Typhoon does not damage its caster"
    );
}

/// Parser regression guard: Typhoon's `DamageEachPlayer` amount is an
/// `ObjectCount { Islands }` whose filter controller is `ScopedPlayer`. If the
/// parser ever regressed to `You` or a non-scoped aggregate, the per-opponent
/// runtime behavior above would silently break — this pins the AST scope.
#[test]
fn typhoon_amount_object_count_is_scoped_player() {
    let def = parse_effect_chain(TYPHOON, AbilityKind::Spell);
    let Effect::DamageEachPlayer { amount, .. } = def.effect.as_ref() else {
        panic!("expected DamageEachPlayer, got {:?}", def.effect);
    };
    let QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount { filter },
    } = amount
    else {
        panic!("expected amount = Ref(ObjectCount {{ .. }}), got {amount:?}");
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected ObjectCount filter = Typed(..), got {filter:?}");
    };
    assert_eq!(
        tf.controller,
        Some(ControllerRef::ScopedPlayer),
        "Typhoon's Island count must be scoped to each opponent (ScopedPlayer), \
         not the caster (You) or a non-scoped aggregate"
    );
}
