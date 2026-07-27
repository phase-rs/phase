//! Reproduction + regression for issue #6375: Goreclaw, Terror of Qal Sisma's
//! power-gated cost reduction.
//!
//! Goreclaw: "Creature spells you cast with power 4 or greater cost {2} less to
//! cast. / Whenever Goreclaw attacks, each creature you control with power 4 or
//! greater gets +1/+1 and gains trample until end of turn."
//!
//! Bug: the cost-reduction static parsed with `spell_filter: null` — the trailing
//! "with power 4 or greater" gate sat after the "spells you cast" infix and blocked
//! the type/subject trims, so the whole "creature ... with power 4+" restriction was
//! dropped and the reduction (mis)applied to EVERY spell. Confirmed reproduction: a
//! power-3 creature spell was wrongly reduced 5→3 at base.
//!
//! CR 208.1 + CR 601.2f: a cost reduction applies only to spells matching the
//! effect's filter, and only creature spells have a power to compare (CR 208.3 — a
//! noncreature card off the battlefield has power only if a P/T is printed on it).
//!
//! Finding #6 (BLOCKING): these tests drive the REAL production cost path
//! (`display_spell_cost`) casting REAL creature card FACES that carry PRINTED power
//! — built through `create_object_from_card_face` / `apply_card_face_to_object` via
//! `add_real_card` — never a hand-constructed object with a force-set `obj.power`.
//! The Goreclaw cost-reduction static is built by parsing the VERBATIM Oracle cost
//! line through `parse_static_line`, so each assertion tests the NEW parser output
//! directly (not a stale card-data fixture).

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::TargetFilter;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Verbatim Goreclaw cost line (issue #6375). Parsed through the real static parser.
const GORECLAW_COST_LINE: &str =
    "Creature spells you cast with power 4 or greater cost {2} less to cast.";

/// Build a board with Goreclaw's parsed cost-reduction static under P0's control,
/// put each named REAL card into P0's hand (printed power comes from the card face),
/// and return the generic mana component the production cost path reports for each.
///
fn goreclaw_generics(spell_names: &[&str]) -> Vec<u32> {
    let db = shared_card_db().expect("Goreclaw regression requires the integration card fixture");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 601.2f: Goreclaw's cost-reduction static, parsed from its verbatim Oracle
    // cost line, on a battlefield permanent P0 controls. `affected` carries the
    // "you cast" controller scope so it applies to spells P0 casts.
    let cost_static =
        parse_static_line(GORECLAW_COST_LINE).expect("Goreclaw cost line must parse to a static");
    scenario
        .add_creature(P0, "Goreclaw, Terror of Qal Sisma", 4, 5)
        .with_static_definition(cost_static);

    // Real card faces in hand — power is populated from the printed P/T by the
    // production face-application path, not force-set (Finding #6).
    let ids: Vec<ObjectId> = spell_names
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Hand, db))
        .collect();

    let mut runner = scenario.build();
    // Finalize public state so the layer pipeline runs and the static-presence
    // index reflects Goreclaw's ModifyCost static (mirrors the Cloud Key cost test).
    // Goreclaw is not in the fixture DB, so face-reapply skips it and its parsed
    // static is preserved.
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let generics = ids
        .iter()
        .map(|&id| {
            let cost = engine::game::casting::display_spell_cost(runner.state(), P0, id)
                .expect("spell should have a displayable cost");
            match cost {
                ManaCost::Cost { generic, .. } => generic,
                other => panic!("expected ManaCost::Cost, got {other:?}"),
            }
        })
        .collect();
    generics
}

/// CR 208.1 + CR 601.2f (#6375): a creature spell with power CLEARLY greater than 4
/// is reduced by {2}. Fire Elemental (5/4, {3}{R}{R}) → generic 3 reduced to 1.
///
/// Reach-guard (not vacuous): the observed 3→1 delta proves the reduction actually
/// fired against a real printed-power creature face, not that the assertion is
/// trivially true. Reverting `spell_filter` to `null` keeps this reduced (null
/// reduces everything) — the sibling NOT-reduced tests are the revert-guards.
#[test]
fn power_five_creature_spell_is_reduced_by_two() {
    let generics = goreclaw_generics(&["Fire Elemental"]);
    assert_eq!(
        generics[0], 1,
        "a power-5 creature spell must be reduced by {{2}} (generic 3 → 1)"
    );
}

/// CR 208.1 + CR 601.2f (#6375): the reported bug. A power-3 creature spell is NOT
/// reduced (power 3 < 4). Hill Giant (3/3, {3}{R}) → generic 3, unchanged.
///
/// Revert-guard: reverting the fix drops the power gate (`spell_filter: null`) and
/// this spell is WRONGLY reduced 3→1 — this assertion then fails (the reported
/// 5→3-at-base bug). Reach-guard: the same board reduces Fire Elemental (power 5)
/// 3→1, proving Goreclaw's static is active and its filter is being evaluated — so
/// Hill Giant's non-reduction is the type/power gate rejecting it, not an inert
/// static.
#[test]
fn power_three_creature_spell_is_not_reduced() {
    let generics = goreclaw_generics(&["Hill Giant", "Fire Elemental"]);
    assert_eq!(
        generics[0], 3,
        "a power-3 creature spell must NOT be reduced (power 3 < 4); reverting the \
         power gate wrongly reduces it 3→1 (issue #6375)"
    );
    // Reach-guard: the static is live and its filter is evaluated on this board.
    assert_eq!(
        generics[1], 1,
        "reach-guard: a power-5 creature spell must be reduced 3→1 on the same board"
    );
}

/// CR 208.3 + CR 601.2f (#6375): a noncreature spell is NOT reduced — a noncreature
/// card off the battlefield has no power (reads 0), so it fails the creature type
/// gate. Arc Lightning (Sorcery, {2}{R}) → generic 2, unchanged.
///
/// Revert-guard: reverting the fix reduces EVERY spell, so this sorcery would be
/// wrongly reduced 2→0 and the assertion fails. Reach-guard: Fire Elemental (a
/// creature spell) is reduced 3→1 on the same board.
#[test]
fn noncreature_spell_is_not_reduced() {
    let generics = goreclaw_generics(&["Arc Lightning", "Fire Elemental"]);
    assert_eq!(
        generics[0], 2,
        "a noncreature spell must NOT be reduced (type gate); reverting the filter \
         wrongly reduces it 2→0 (issue #6375)"
    );
    // Reach-guard: the static is live and its filter is evaluated on this board.
    assert_eq!(
        generics[1], 1,
        "reach-guard: a power-5 creature spell must be reduced 3→1 on the same board"
    );
}

/// CR 208.1 + CR 601.2f (#6375): boundary — power EXACTLY 4 satisfies the GE gate
/// and is reduced. Onakke Ogre (4/2, {2}{R}) → generic 2 reduced to 0.
///
/// Reach-guard: the observed 2→0 delta proves the GE-4 gate admits the boundary
/// value against a real printed-power (exactly 4) creature face.
#[test]
fn power_exactly_four_creature_spell_is_reduced_boundary() {
    let generics = goreclaw_generics(&["Onakke Ogre"]);
    assert_eq!(
        generics[0], 0,
        "a power-4 creature spell must be reduced by {{2}} (GE boundary; generic 2 → 0)"
    );
}

/// Cast-cost probe for a single named spell in P0's hand, with `graveyard` real
/// cards seeded into P0's graveyard FIRST so a graveyard-counting CDA (Tarmogoyf:
/// power = number of card types among cards in all graveyards) resolves against
/// them. Mirrors `goreclaw_generics` but returns the one probed spell's generic.
fn goreclaw_generic_with_graveyard(spell_name: &str, graveyard: &[&str]) -> u32 {
    let db = shared_card_db().expect("Goreclaw regression requires the integration card fixture");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cost_static =
        parse_static_line(GORECLAW_COST_LINE).expect("Goreclaw cost line must parse to a static");
    scenario
        .add_creature(P0, "Goreclaw, Terror of Qal Sisma", 4, 5)
        .with_static_definition(cost_static);

    // Real cards of DISTINCT card types in the graveyard set Tarmogoyf's CDA
    // power to exactly `graveyard.len()`.
    for name in graveyard {
        scenario.add_real_card(P0, name, Zone::Graveyard, db);
    }

    let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let cost = engine::game::casting::display_spell_cost(runner.state(), P0, spell)
        .expect("spell should have a displayable cost");
    match cost {
        ManaCost::Cost { generic, .. } => generic,
        other => panic!("expected ManaCost::Cost, got {other:?}"),
    }
}

/// CR 208.2a + CR 613.4b (#6375, MED-1): a dynamic-P/T creature spell must gate on
/// its CHARACTERISTIC-DEFINING power, which functions in every zone — not on the
/// stale 0 that a `*` card stores off the battlefield. Tarmogoyf ({1}{G}, power =
/// number of card types among all graveyards) with a four-distinct-type graveyard
/// (Creature + Instant + Sorcery + Artifact) has CDA power 4, so Goreclaw reduces
/// it: generic 1 → 0.
///
/// Revert-guard: reverting the CDA resolution (`spell_object_effective_pt_value` →
/// the raw `object_pt_value`) reads Tarmogoyf's stored power as 0, fails the GE-4
/// gate, and leaves the generic at 1 — this assertion then fails. The power-3
/// sibling below is the discriminating lower bound.
#[test]
fn dynamic_cda_power_four_creature_spell_is_reduced() {
    let generic = goreclaw_generic_with_graveyard(
        "Tarmogoyf",
        &[
            "Absolver Thrull",
            "Abrade",
            "Arc Lightning",
            "Amulet of Vigor",
        ],
    );
    assert_eq!(
        generic, 0,
        "a CDA creature spell whose CDA power is 4 must be reduced by {{2}} \
         (generic 1 → 0); reverting the CDA resolution reads power 0 and wrongly \
         leaves it unreduced at 1 (issue #6375 MED-1)"
    );
}

/// CR 208.2a (#6375, MED-1): the discriminating lower bound. The SAME Tarmogoyf
/// with a three-distinct-type graveyard (Instant + Sorcery + Artifact) has CDA
/// power 3 (< 4) and is NOT reduced: generic 1 stays 1. Together with the power-4
/// sibling this proves the gate reads the true CDA magnitude (3 vs 4), not merely
/// "nonzero".
#[test]
fn dynamic_cda_power_three_creature_spell_is_not_reduced() {
    let generic = goreclaw_generic_with_graveyard(
        "Tarmogoyf",
        &["Abrade", "Arc Lightning", "Amulet of Vigor"],
    );
    assert_eq!(
        generic, 1,
        "a CDA creature spell whose CDA power is 3 must NOT be reduced (3 < 4); \
         the power-4 sibling on a four-type graveyard IS reduced"
    );
}

/// CR 208.1 (#6375, MED-1): fixed-P/T control. Fire Elemental (fixed power 5,
/// {3}{R}{R}) on the SAME three-type graveyard as the power-3 CDA test is still
/// reduced 3 → 1 — its printed power is unaffected by CDA resolution. This proves
/// the new resolution path changes nothing for a fixed-P/T spell (it must return
/// the stored `object_pt_value` unchanged when the object sources no self-CDA).
#[test]
fn fixed_power_control_reduced_regardless_of_graveyard() {
    let generic = goreclaw_generic_with_graveyard(
        "Fire Elemental",
        &["Abrade", "Arc Lightning", "Amulet of Vigor"],
    );
    assert_eq!(
        generic, 1,
        "a fixed power-5 creature spell is reduced 3 → 1 regardless of graveyard \
         (control: CDA resolution must not change fixed-P/T behavior)"
    );
}

/// Cast-cost probe for a real creature-card FACE in P0's hand that carries an
/// attached conditional **constant**-P/T CDA, parsed from a verbatim Oracle line
/// via `parse_static_line` (the Angry Mob off-turn form at
/// `oracle_static/tests.rs`: `SetPower { value }` / `SetToughness { value }`
/// gated on a turn-window `StaticCondition`).
///
/// The CDA is attached AFTER `rehydrate_game_from_card_db` — which re-applies the
/// printed face and would otherwise drop any extra static on a fixture card —
/// onto both `static_definitions` and `base_static_definitions`, exactly as
/// `GameScenario::with_static_definition` does. The spell's power is still
/// derived through the production `spell_object_effective_pt_value` path from the
/// CDA (via `display_spell_cost`), never a force-set `obj.power` (Finding #6).
fn goreclaw_generic_with_constant_cda(spell_name: &str, cda_line: &str) -> u32 {
    let db = shared_card_db().expect("Goreclaw regression requires the integration card fixture");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cost_static =
        parse_static_line(GORECLAW_COST_LINE).expect("Goreclaw cost line must parse to a static");
    scenario
        .add_creature(P0, "Goreclaw, Terror of Qal Sisma", 4, 5)
        .with_static_definition(cost_static);

    let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Parse the conditional constant-P/T CDA from its Oracle line. Fixture
    // self-check: it must be the exact SelfRef constant-P/T CDA form the review
    // references (characteristic-defining, SelfRef-affected, `SetPower`/
    // `SetToughness { value }` set variants under a turn-window condition).
    let cda = parse_static_line(cda_line)
        .expect("conditional constant-P/T CDA line must parse to static");
    assert_eq!(
        cda.affected,
        Some(TargetFilter::SelfRef),
        "constant-P/T CDA must be SelfRef-affected: {cda:?}"
    );
    assert!(
        cda.characteristic_defining,
        "constant-P/T CDA must be characteristic-defining: {cda:?}"
    );
    assert!(
        cda.condition.is_some(),
        "the CDA line under test must carry a turn-window condition: {cda:?}"
    );

    let obj = runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell object must exist post-rehydrate");
    obj.static_definitions.push(cda.clone());
    std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(cda);

    let cost = engine::game::casting::display_spell_cost(runner.state(), P0, spell)
        .expect("spell should have a displayable cost");
    match cost {
        ManaCost::Cost { generic, .. } => generic,
        other => panic!("expected ManaCost::Cost, got {other:?}"),
    }
}

/// CR 604.3 + CR 613.4a + CR 601.2f (#6375 HIGH): a creature spell whose ACTIVE
/// conditional **constant**-P/T CDA sets power to 4 must gate on that CDA power —
/// the fixed-constant `SetPower` set CDA functions off the battlefield exactly
/// like the dynamic setter. Hill Giant ({3}{R}, printed 3/3) carries a
/// "During your turn, ~'s power and toughness are each 4." CDA; the scenario is
/// at P0's PreCombatMain, so `DuringYourTurn` holds, the constant CDA is active,
/// effective power is 4, and Goreclaw reduces it: generic 3 → 1.
///
/// Revert-guard (RED carrier): reverting the new fixed `SetPower { value }` /
/// `SetToughness { value }` set-stage arm in `spell_object_effective_pt_value`
/// makes the resolver skip the constant CDA and read Hill Giant's printed power 3
/// (< 4). The GE-4 gate then rejects it and the generic stays 3 — this assertion
/// fails. That is exactly the off-battlefield "evaluated as printed" bug the
/// review flagged.
#[test]
fn conditional_constant_cda_active_power_four_creature_spell_is_reduced() {
    let generic = goreclaw_generic_with_constant_cda(
        "Hill Giant",
        "During your turn, Hill Giant's power and toughness are each 4.",
    );
    assert_eq!(
        generic, 1,
        "an ACTIVE conditional constant-P/T CDA setting power 4 must be reduced by \
         {{2}} (generic 3 → 1); reverting the new fixed `SetPower` set-stage arm \
         reads printed power 3, fails the GE-4 gate, and wrongly leaves it at 3 \
         (issue #6375 HIGH)"
    );
}

/// CR 611.3a + CR 604.3 (#6375 HIGH): the discriminating condition-gating case. A
/// conditional constant-P/T CDA must apply ONLY while its condition holds. The
/// SAME Hill Giant carries a "During turns other than yours, ~'s power and
/// toughness are each 4." CDA (gated `Not{DuringYourTurn}`). On P0's own turn the
/// off-window condition is FALSE, so the CDA does not function — the spell reads
/// its printed power 3 (< 4) and is NOT reduced: generic stays 3.
///
/// Revert-guard: applying the constant CDA UNCONDITIONALLY (dropping the
/// condition gate mirrored from the layer authority) would set power to 4 and
/// wrongly reduce the spell 3 → 1 — this assertion then fails. Together with the
/// active sibling above this proves the resolver both applies the fixed constant
/// CDA and honors its condition, never applying it outside its window.
#[test]
fn conditional_constant_cda_off_window_creature_spell_is_not_reduced() {
    let generic = goreclaw_generic_with_constant_cda(
        "Hill Giant",
        "During turns other than yours, Hill Giant's power and toughness are each 4.",
    );
    assert_eq!(
        generic, 3,
        "an off-window conditional constant-P/T CDA must NOT apply — the spell reads \
         printed power 3 and is not reduced; applying the constant CDA \
         unconditionally would set power 4 and wrongly reduce it to 1 (condition \
         gating, issue #6375 HIGH)"
    );
}
