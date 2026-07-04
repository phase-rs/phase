//! Regression for GitHub issue #699 — the "boost-then-fight" one-sided-fight
//! class (Ambuscade and ~18 siblings).
//!
//! Oracle (Ambuscade): "Target creature you control gets +1/+1 until end of
//! turn. It deals damage equal to its power to target creature an opponent
//! controls."
//!
//! The bug: PR #3530 rebound the anaphoric "its power" amount from
//! `Power{Anaphoric}` to `Power{Target}` and (in an earlier form) clobbered the
//! damage recipient to `ParentTarget`, which made the boosted creature damage
//! *itself*. The approved Option-b fix leaves the AST amount as
//! `Power{Anaphoric}` (keeping the serialized export in sync with the Oracle
//! "its"), keeps `damage_source = Some(Target)` + the fresh-opponent recipient,
//! and resolves `Power{Anaphoric}` to `targets[0]` (the boosted creature) at
//! runtime via the one-sided-fight fallback in `game/quantity.rs`.
//!
//! These tests parse the real Oracle text through `add_spell_to_hand_from_oracle`
//! (the production parser path the fix modifies) and drive the full cast
//! pipeline, so they exercise parser + runtime end-to-end. They would fail if
//! either half of the fix were reverted:
//!   - revert the parser → the recipient/source desyncs and the boosted
//!     creature damages itself (or reads the wrong power).
//!   - revert the `game/quantity.rs` Anaphoric fallback → `Power{Anaphoric}`
//!     resolves to 0 (no effect-context/event/cost referent), so the opponent
//!     takes 0 instead of the boosted power.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

const AMBUSCADE_ORACLE: &str = "Target creature you control gets +1/+1 until end of turn. \
It deals damage equal to its power to target creature an opponent controls.";

// A "Then it deals..." conditional-boost variant whose damage subject is
// classified `SelfRef`, so the parser must coerce the "its power" amount
// Source -> Anaphoric (step 1b of the fix) for the runtime fallback to fire.
const CONDITIONAL_FIGHT_ORACLE: &str = "Target creature you control gets +2/+0 until end of turn. \
Then it deals damage equal to its power to target creature an opponent controls.";

/// CR 120.1 + CR 608.2c + CR 115.10a: Ambuscade boosts P0's 2/2 to 3/3, then the
/// boosted creature (NOT the spell, NOT the opponent's creature) deals damage
/// equal to its own current power (3) to the opponent's 3/3. The boosted
/// creature takes no damage — it is the source, the opponent's creature is the
/// recipient.
///
/// Discriminating assertion: the opponent's creature ends with 3 marked damage
/// (= boosted power) and P0's creature ends with 0. If the runtime Anaphoric
/// fallback is reverted, the amount resolves to 0 and the opponent takes 0; if
/// the parser recipient is reverted, the boosted creature damages itself.
#[test]
fn ambuscade_boosted_creature_deals_its_power_to_opponent_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ambuscade", true, AMBUSCADE_ORACLE)
        .id();
    // P0's own creature to boost (2/2 -> 3/3 after +1/+1).
    let own = scenario.add_creature(P0, "Boosted Beast", 2, 2).id();
    // The opponent's creature that takes the fight damage. Power 7 (distinct
    // from the boosted creature's 3) so foe_damage discriminates "its power" =
    // boosted source (3) vs accidentally = recipient (7).
    let foe = scenario.add_creature(P1, "Opposing Bear", 7, 7).id();

    let mut runner = scenario.build();

    // Targets are matched to slots in written order (CR 601.2c): slot 1 (Pump on
    // a You-controlled creature) -> own; slot 2 (DealDamage on an opponent
    // creature) -> foe.
    let outcome = runner.cast(spell).target_objects(&[own, foe]).resolve();

    let foe_damage = outcome.state().objects[&foe].damage_marked;
    let own_damage = outcome.state().objects[&own].damage_marked;

    assert_eq!(
        foe_damage, 3,
        "#699: the boosted creature must deal its OWN power (3 = 2 base + 1) to \
         the opponent's creature; got {foe_damage}. A value of 0 means the \
         Power{{Anaphoric}} runtime fallback was reverted."
    );
    assert_eq!(
        own_damage, 0,
        "#699: the boosted creature is the damage SOURCE, not a recipient — it \
         must take 0; got {own_damage}. Non-zero means the recipient was \
         clobbered to the boosted creature (self-damage)."
    );
}

/// Step 1b coverage — the "Then it deals..." SelfRef-subject variant. The parser
/// must coerce the prematurely-Source-bound "its power" amount to
/// `Power{Anaphoric}` (NOT `Source`, which would read the spell, power 0). With
/// +2/+0 the boosted 2/2 becomes a 4/2 and deals 4 to the opponent's 5/5; the
/// boosted creature takes 0.
#[test]
fn conditional_then_it_deals_variant_reads_boosted_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Conditional Strike", true, CONDITIONAL_FIGHT_ORACLE)
        .id();
    let own = scenario.add_creature(P0, "Pumped Hunter", 2, 2).id();
    let foe = scenario.add_creature(P1, "Sturdy Ogre", 5, 5).id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[own, foe]).resolve();

    let foe_damage = outcome.state().objects[&foe].damage_marked;
    let own_damage = outcome.state().objects[&own].damage_marked;

    assert_eq!(
        foe_damage, 4,
        "step 1b: the boosted creature (2 base + 2 = 4 power) must deal 4 to the \
         opponent; got {foe_damage}. 0 means the Source->Anaphoric coercion (or \
         the runtime fallback) was reverted."
    );
    assert_eq!(
        own_damage, 0,
        "step 1b: the boosted creature is the source and must take 0; got {own_damage}."
    );
}

/// Real-card confirmation (Bite Down on Crime — the printed fight sentences,
/// "you don't control" == Opponent recipient). The boost head and the "It
/// deals" clause are SEPARATE sentences with no condition; the boosted 3/3
/// (1 base + 2) deals 3 to the opponent's 6/6 and takes 0. Discriminates
/// boosted source power (3) from the recipient's own power (6). Proves the
/// runtime one-sided-fight source prepend (parent's chosen creature ->
/// `targets[0]`) on a real card's exact Oracle text, not a synthetic proxy.
const BITE_DOWN_ON_CRIME_FIGHT: &str = "Target creature you control gets +2/+0 until end of turn. \
It deals damage equal to its power to target creature you don't control.";

/// Reproduction probe for GitHub #4234 (plain "Bite Down", the NO-BOOST
/// variant): "Target creature you control deals damage equal to its power to
/// target creature or planeswalker you don't control." The card legitimately
/// targets TWO objects (CR 601.2c) — the source creature you control and the
/// opponent's recipient — so "asks for 2 targets" is correct. The only real-bug
/// question is whether the SOURCE wrongly takes damage too. P0's 4/4 deals 4 to
/// the opponent's 6/6 and must take 0 itself.
#[test]
fn bite_down_no_boost_source_deals_power_and_takes_none() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    const BITE_DOWN_NO_BOOST: &str = "Target creature you control deals damage equal to its power \
to target creature or planeswalker you don't control.";

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bite Down", true, BITE_DOWN_NO_BOOST)
        .id();
    let own = scenario.add_creature(P0, "Biting Beast", 4, 4).id();
    let foe = scenario.add_creature(P1, "Opposing Ogre", 6, 6).id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[own, foe]).resolve();

    let foe_damage = outcome.state().objects[&foe].damage_marked;
    let own_damage = outcome.state().objects[&own].damage_marked;

    assert_eq!(
        foe_damage, 4,
        "#4234: the source creature (power 4) must deal 4 to the opponent's \
         creature; got {foe_damage}."
    );
    assert_eq!(
        own_damage, 0,
        "#4234: the source creature must take NO damage (it is the damage source, \
         not a recipient); got {own_damage}. Non-zero means the source slot is \
         being damaged as well — the reported double-damage bug."
    );
}

#[test]
fn bite_down_on_crime_boosted_creature_deals_its_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bite Down on Crime", true, BITE_DOWN_ON_CRIME_FIGHT)
        .id();
    let own = scenario.add_creature(P0, "Brave Beast", 1, 3).id();
    let foe = scenario.add_creature(P1, "Hardy Ogre", 6, 6).id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[own, foe]).resolve();

    let foe_damage = outcome.state().objects[&foe].damage_marked;
    let own_damage = outcome.state().objects[&own].damage_marked;

    assert_eq!(
        foe_damage, 3,
        "the boosted creature (1 base + 2 = 3 power) must deal 3 to the opponent; \
         got {foe_damage}. A value of 6 means the source/amount read the \
         recipient instead of the boosted creature (the source prepend was reverted)."
    );
    assert_eq!(
        own_damage, 0,
        "the boosted creature is the source and must take 0; got {own_damage}."
    );
}
